use crate::{
    State,
    error::{Error, Result},
    model::{self, User},
};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json,
    extract::State as AxumState,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

pub fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| v.trim().strip_prefix("amalthea_session="))
}
pub async fn user(app: &State, headers: &HeaderMap) -> Result<User> {
    let token = cookie_token(headers).ok_or_else(Error::unauthorized)?;
    sqlx::query_as("SELECT u.id, u.username, u.quota_bytes FROM users u JOIN sessions s ON s.user_id = u.id WHERE s.token_hash = ? AND s.expires_at > ?")
        .bind(model::hash(token)).bind(model::now()).fetch_optional(&app.db).await?.ok_or_else(Error::unauthorized)
}
fn cookie(app: &State, token: &str, age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "amalthea_session={token}; Path={}/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        app.settings.prefix(),
        if app.settings.webroot.starts_with("https:") {
            "; Secure"
        } else {
            ""
        }
    ))
    .expect("safe cookie")
}
async fn session(app: &State, user: &User) -> Result<Response> {
    let token = model::secret();
    let age = app.settings.session_days * 86400;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(model::hash(&token))
        .bind(user.id)
        .bind(model::now() + age)
        .execute(&app.db)
        .await?;
    Ok(([(header::SET_COOKIE, cookie(app, &token, age))], Json(user)).into_response())
}

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
}

pub async fn login(
    AxumState(app): AxumState<State>,
    Json(input): Json<Credentials>,
) -> Result<Response> {
    if !model::valid_username(&input.username) || input.password.len() > 1024 {
        return Err(Error::bad("Invalid username or password"));
    }
    let key = input.username.to_ascii_lowercase();
    {
        let mut attempts = app.login_attempts.lock().await;
        attempts.retain(|_, (at, _)| *at > model::now() - 600);
        if attempts.len() > 10000 {
            return Err(Error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many login attempts; try later".into(),
            ));
        }
        let entry = attempts.entry(key.clone()).or_insert((model::now(), 0));
        if entry.1 >= 20 {
            return Err(Error(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many login attempts; wait ten minutes".into(),
            ));
        }
        entry.1 += 1;
    }
    let _permit = app.password_jobs.try_acquire().map_err(|_| {
        Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Sign-in busy; please retry shortly".into(),
        )
    })?;
    let record: Option<(i64, String, i64, String)> = sqlx::query_as(
        "SELECT id, username, quota_bytes, password_hash FROM users WHERE username = ?",
    )
    .bind(&input.username)
    .fetch_optional(&app.db)
    .await?;
    // Use the same expensive verifier for unknown usernames.
    let encoded = record.as_ref().map(|r| r.3.clone());
    let valid = tokio::task::spawn_blocking(move || {
        if let Some(encoded) = encoded {
            PasswordHash::new(&encoded).is_ok_and(|h| {
                Argon2::default()
                    .verify_password(input.password.as_bytes(), &h)
                    .is_ok()
            })
        } else {
            let _ = Argon2::default()
                .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng));
            false
        }
    })
    .await?;
    if !valid {
        return Err(Error(
            StatusCode::UNAUTHORIZED,
            "Invalid username or password".into(),
        ));
    }
    app.login_attempts.lock().await.remove(&key);
    let (id, username, quota_bytes, _) = record.ok_or_else(Error::unauthorized)?;
    session(
        &app,
        &User {
            id,
            username,
            quota_bytes,
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct UsernameCheck {
    token: String,
    username: String,
}

pub async fn check_username(
    AxumState(app): AxumState<State>,
    Json(input): Json<UsernameCheck>,
) -> Result<Json<serde_json::Value>> {
    let invited: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM invitations WHERE token_hash = ? AND expires_at > ?)",
    )
    .bind(model::hash(&input.token))
    .bind(model::now())
    .fetch_one(&app.db)
    .await?;
    if !invited {
        return Err(Error::bad("Invitation is invalid, expired or already used"));
    }
    let mut available = model::valid_username(&input.username);
    if available {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = ?)")
                .bind(&input.username)
                .fetch_one(&app.db)
                .await?;
        let directory = app.settings.data_dir.join("users").join(&input.username);
        let occupied = match tokio::fs::symlink_metadata(directory).await {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        available = !exists && !occupied;
    }
    Ok(Json(serde_json::json!({ "available": available })))
}

#[derive(Deserialize)]
pub struct Registration {
    token: String,
    username: String,
    password: String,
}
pub async fn register(
    AxumState(app): AxumState<State>,
    Json(input): Json<Registration>,
) -> Result<Response> {
    if !model::valid_username(&input.username) {
        return Err(Error::bad(
            "Use 1–64 letters, numbers or underscores; this username may be reserved",
        ));
    }
    if !(12..=1024).contains(&input.password.len()) {
        return Err(Error::bad("Password must contain 12–1024 bytes"));
    }
    let token_hash = model::hash(&input.token);
    let quota: Option<i64> = sqlx::query_scalar(
        "SELECT quota_bytes FROM invitations WHERE token_hash = ? AND expires_at > ?",
    )
    .bind(&token_hash)
    .bind(model::now())
    .fetch_optional(&app.db)
    .await?;
    if quota.is_none() {
        return Err(Error::bad("Invitation is invalid, expired or already used"));
    }
    let _permit = app.password_jobs.try_acquire().map_err(|_| {
        Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Registration busy; please retry shortly".into(),
        )
    })?;
    let encoded = tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|hash| hash.to_string())
            .map_err(Error::internal)
    })
    .await??;
    let _guard = app.mutation.lock().await;
    let mut tx = app.db.begin().await?;
    let quota: Option<i64> = sqlx::query_scalar(
        "DELETE FROM invitations WHERE token_hash = ? AND expires_at > ? RETURNING quota_bytes",
    )
    .bind(&token_hash)
    .bind(model::now())
    .fetch_optional(&mut *tx)
    .await?;
    let quota =
        quota.ok_or_else(|| Error::bad("Invitation is invalid, expired or already used"))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = ?)")
        .bind(&input.username)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(Error::conflict("Username is already taken"));
    }
    let dir = app.settings.data_dir.join("users").join(&input.username);
    // Never adopt an existing folder into a new account.
    tokio::fs::create_dir(&dir).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::conflict("A data directory already exists for this username; ask the operator")
        } else {
            e.into()
        }
    })?;
    let result = sqlx::query(
        "INSERT INTO users (username, password_hash, quota_bytes, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&input.username)
    .bind(encoded)
    .bind(quota)
    .bind(model::now())
    .execute(&mut *tx)
    .await;
    let id = match result {
        Ok(row) => row.last_insert_rowid(),
        Err(e) => {
            let _ = tokio::fs::remove_dir(&dir).await;
            return Err(e.into());
        }
    };
    tx.commit().await?;
    session(
        &app,
        &User {
            id,
            username: input.username,
            quota_bytes: quota,
        },
    )
    .await
}

pub async fn logout(AxumState(app): AxumState<State>, headers: HeaderMap) -> Result<Response> {
    if let Some(token) = cookie_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(model::hash(token))
            .execute(&app.db)
            .await?;
    }
    Ok((
        [(header::SET_COOKIE, cookie(&app, "", 0))],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}
