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

pub fn cookie_name(settings: &crate::configuration::Settings) -> String {
    if settings.webroot.starts_with("https:") {
        // __Host- forbids Domain and requires Path=/. A mount-specific name
        // keeps separate installations from overwriting each other's cookies.
        format!(
            "__Host-amalthea_session_{}",
            &model::hash(settings.prefix())[..16]
        )
    } else {
        "amalthea_session".into()
    }
}
pub fn cookie_token<'a>(app: &State, headers: &'a HeaderMap) -> Option<&'a str> {
    let prefix = format!("{}=", cookie_name(&app.settings));
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| v.trim().strip_prefix(&prefix))
}
pub async fn user(app: &State, headers: &HeaderMap) -> Result<User> {
    let token = cookie_token(app, headers).ok_or_else(Error::unauthorized)?;
    sqlx::query_as("SELECT u.id, u.username, u.quota_bytes FROM users u JOIN sessions s ON s.user_id = u.id WHERE s.token_hash = ? AND s.expires_at > ?")
        .bind(model::hash(token)).bind(model::now()).fetch_optional(&app.db).await?.ok_or_else(Error::unauthorized)
}
fn cookie(app: &State, token: &str, age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={token}; Path={}; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        cookie_name(&app.settings),
        if app.settings.webroot.starts_with("https:") {
            "/".to_owned()
        } else {
            format!("{}/", app.settings.prefix())
        },
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
    // Serialize pruning and insertion so concurrent logins cannot exceed 32.
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND (expires_at <= ? OR token_hash NOT IN (SELECT token_hash FROM sessions WHERE user_id = ? ORDER BY expires_at DESC, rowid DESC LIMIT 31))")
        .bind(user.id).bind(model::now()).bind(user.id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(model::hash(&token))
        .bind(user.id)
        .bind(model::now() + age)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(([(header::SET_COOKIE, cookie(app, &token, age))], Json(user)).into_response())
}

async fn password_job<T: Send + 'static>(
    permit: tokio::sync::OwnedSemaphorePermit,
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    // Blocking Argon2 work continues after request cancellation. Its permit
    // must remain held until the blocking worker actually exits.
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await?)
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
    let permit = app.password_jobs.clone().try_acquire_owned().map_err(|_| {
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
    let valid = password_job(permit, move || {
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

#[derive(sqlx::FromRow)]
struct Invitation {
    username: Option<String>,
    quota_bytes: i64,
}
impl Invitation {
    fn allows(&self, username: &str) -> bool {
        self.username
            .as_deref()
            .is_none_or(|assigned| assigned == username)
    }
}

async fn load_invitation(app: &State, token_hash: &str) -> Result<Invitation> {
    sqlx::query_as(
        "SELECT username, quota_bytes FROM invitations WHERE token_hash = ? AND expires_at > ?",
    )
    .bind(token_hash)
    .bind(model::now())
    .fetch_optional(&app.db)
    .await?
    .ok_or_else(|| Error::bad("Invitation is invalid, expired or already used"))
}

#[derive(Deserialize)]
pub struct InvitationCheck {
    token: String,
}

pub async fn invitation(
    AxumState(app): AxumState<State>,
    Json(input): Json<InvitationCheck>,
) -> Result<Json<serde_json::Value>> {
    let invitation = load_invitation(&app, &model::hash(&input.token)).await?;
    Ok(Json(serde_json::json!({ "username": invitation.username })))
}

pub async fn username_available(
    app: &State,
    connection: &mut sqlx::SqliteConnection,
    username: &str,
    invitation_hash: &str,
) -> Result<bool> {
    if !model::valid_username(username) {
        return Ok(false);
    }
    let occupied: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE username = ?) OR EXISTS(SELECT 1 FROM invitations WHERE username = ? AND expires_at > ? AND token_hash != ?)",
    )
    .bind(username).bind(username).bind(model::now()).bind(invitation_hash)
    .fetch_one(connection).await?;
    if occupied {
        return Ok(false);
    }
    let directory = app.settings.data_dir.join("users").join(username);
    match tokio::fs::symlink_metadata(directory).await {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e.into()),
    }
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
    let token_hash = model::hash(&input.token);
    let invitation = load_invitation(&app, &token_hash).await?;
    let mut connection = app.db.acquire().await?;
    let available = invitation.allows(&input.username)
        && username_available(&app, &mut connection, &input.username, &token_hash).await?;
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
            "Use 1-64 ASCII letters, numbers, _, ., -, @ or +; no leading dot",
        ));
    }
    if !(12..=1024).contains(&input.password.len()) {
        return Err(Error::bad("Password must contain 12-1024 bytes"));
    }
    let token_hash = model::hash(&input.token);
    let invitation = load_invitation(&app, &token_hash).await?;
    if !invitation.allows(&input.username) {
        return Err(Error::bad("Username must match the invitation"));
    }
    let permit = app.password_jobs.clone().try_acquire_owned().map_err(|_| {
        Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Registration busy; please retry shortly".into(),
        )
    })?;
    let encoded = password_job(permit, move || {
        Argon2::default()
            .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|hash| hash.to_string())
            .map_err(Error::internal)
    })
    .await??;
    let _guard = app.mutation.lock().await;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let invitation: Invitation = sqlx::query_as(
        "DELETE FROM invitations WHERE token_hash = ? AND expires_at > ? RETURNING username, quota_bytes",
    )
    .bind(&token_hash)
    .bind(model::now())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::bad("Invitation is invalid, expired or already used"))?;
    // Verify the binding again inside the transaction that consumes the invitation.
    if !invitation.allows(&input.username) {
        return Err(Error::bad("Username must match the invitation"));
    }
    if !username_available(&app, &mut tx, &input.username, &token_hash).await? {
        return Err(Error::conflict(
            "Username is already taken, assigned to another invitation, or has an existing data directory",
        ));
    }
    let quota = invitation.quota_bytes;
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
    if let Some(token) = cookie_token(&app, &headers) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_request_keeps_password_slot_until_worker_exits() {
        let jobs = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = jobs.clone().try_acquire_owned().unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let request = tokio::spawn(password_job(permit, move || {
            started.send(()).unwrap();
            let _ = wait.recv();
        }));
        ready.await.unwrap();
        request.abort();
        let cancelled = request.await.unwrap_err().is_cancelled();
        let blocked = jobs.try_acquire().is_err();
        release.send(()).unwrap();
        assert!(cancelled && blocked);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(2), jobs.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
    }

    #[tokio::test]
    async fn concurrent_sessions_are_capped_and_evicted_tokens_stop_working() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let app = crate::App::open(crate::configuration::Settings {
            data_dir: dir.path().join("data"),
            static_dir: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/static"),
            ..Default::default()
        })
        .await
        .unwrap();
        sqlx::query("INSERT INTO users(id, username, password_hash, quota_bytes, created_at) VALUES (1, 'Alice', 'unused', 0, 0), (2, 'Bob', 'unused', 0, 0)")
            .execute(&app.db).await.unwrap();
        sqlx::query("INSERT INTO sessions(token_hash, user_id, expires_at) VALUES (?, 1, ?), (?, 2, ?), ('expired', 1, 0)")
            .bind(model::hash("old")).bind(model::now()+60)
            .bind(model::hash("other")).bind(model::now()+60).execute(&app.db).await.unwrap();
        let mut jobs = tokio::task::JoinSet::new();
        for _ in 0..35 {
            let app = app.clone();
            jobs.spawn(async move {
                session(
                    &app,
                    &User {
                        id: 1,
                        username: "Alice".into(),
                        quota_bytes: 0,
                    },
                )
                .await
            });
        }
        while let Some(result) = jobs.join_next().await {
            result.unwrap().unwrap();
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = 1")
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 32);
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "amalthea_session=old".parse().unwrap());
        assert_eq!(
            user(&app, &headers).await.unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
        headers.insert(header::COOKIE, "amalthea_session=other".parse().unwrap());
        assert_eq!(user(&app, &headers).await.unwrap().id, 2);
        let response = session(
            &app,
            &User {
                id: 1,
                username: "Alice".into(),
                quota_bytes: 0,
            },
        )
        .await
        .unwrap();
        headers.insert(
            header::COOKIE,
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap(),
        );
        assert_eq!(user(&app, &headers).await.unwrap().id, 1);
    }
}
