use crate::{
    State, auth,
    error::{Error, Result},
    model::{self, FileRecord, LinkRecord, User},
    storage, transfers,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State as AxumState},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::services::ServeDir;

pub fn router(app: State) -> Router {
    let api = Router::new()
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .route("/register/check-username", post(auth::check_username))
        .route("/logout", post(auth::logout))
        .route("/me", get(me))
        .route("/files", get(files))
        .route(
            "/files/{id}",
            get(file_meta).patch(rename).delete(delete_file),
        )
        .route("/files/{id}/content", get(download))
        .route("/files/{id}/retry", post(retry_processing))
        .route("/files/{id}/links", get(links).post(create_link))
        .route("/links/{id}", axum::routing::delete(revoke_link))
        .route("/links/{id}/logs", get(logs))
        .route("/uploads", get(transfers::list).post(transfers::start))
        .route(
            "/uploads/{id}",
            get(transfers::status)
                .put(transfers::chunk)
                .delete(transfers::cancel),
        )
        .route("/uploads/{id}/finish", post(transfers::finish))
        .route("/public/{token}", get(public_meta))
        .route("/public/{token}/content", get(public_download))
        .layer(DefaultBodyLimit::max(transfers::CHUNK_BYTES));
    let routes = Router::new()
        .nest("/api/live", api)
        .route("/health", get(|| async { "ok" }))
        .nest_service(
            "/res/static",
            ServeDir::new(app.settings.static_dir.clone()),
        )
        .fallback(page)
        .layer(middleware::from_fn_with_state(app.clone(), security))
        .with_state(app.clone());
    if app.settings.prefix().is_empty() {
        routes
    } else {
        let prefix = app.settings.prefix().to_owned();
        let canonical = format!("{prefix}/");
        Router::new()
            .route(
                &prefix,
                get(move || async move { Redirect::permanent(&canonical) }),
            )
            .nest(&prefix, routes)
    }
}

async fn security(AxumState(app): AxumState<State>, request: Request, next: Next) -> Response {
    if !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        let headers = request.headers();
        if headers.get("x-amalthea").is_none_or(|h| h != "1")
            || headers
                .get(header::ORIGIN)
                .is_some_and(|h| h != app.settings.origin().as_str())
            || headers
                .get("sec-fetch-site")
                .is_some_and(|h| h == "cross-site")
        {
            return Error(
                StatusCode::FORBIDDEN,
                "Same-origin requests with X-Amalthea: 1 are required".into(),
            )
            .into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    // Solid's noncompiled html tag uses Function to instantiate developer-owned templates.
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response
}

async fn page(
    AxumState(app): AxumState<State>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return Err(Error::missing());
    }
    let original = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path())
        .unwrap_or(request.uri().path());
    let path = original
        .strip_prefix(app.settings.prefix())
        .unwrap_or(original);
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if matches!(path, "/login/" | "/register/") {
        return Ok(Redirect::permanent(&format!(
            "{}{}",
            app.settings.prefix(),
            path.trim_end_matches('/')
        ))
        .into_response());
    }
    let imprint = match &app.settings.imprint {
        None => None,
        Some(path) => match tokio::fs::read_to_string(path).await {
            Ok(content) if !content.trim().is_empty() => Some(content),
            Ok(_) => None,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        },
    };
    if matches!(path, "/imprint" | "/imprint/") && imprint.is_none() {
        return Err(Error::missing());
    }
    let valid = path == "/"
        || matches!(path, "/imprint" | "/imprint/")
        || path == "/login"
        || path == "/register"
        || (parts.len() == 3 && parts[0] == "s");
    if !valid {
        if parts.len() != 1 {
            return Err(Error::missing());
        }
        match auth::user(&app, &headers).await {
            Ok(user) if user.username.eq_ignore_ascii_case(parts[0]) => {
                let canonical = format!("/{}/", user.username);
                if path != canonical {
                    return Ok(Redirect::permanent(&format!(
                        "{}{canonical}",
                        app.settings.prefix()
                    ))
                    .into_response());
                }
            }
            _ => {
                return Ok(
                    Redirect::to(&format!("{}/login", app.settings.prefix())).into_response()
                );
            }
        }
    } else if path != "/" && path.ends_with('/') {
        return Ok(Redirect::permanent(&format!(
            "{}{}",
            app.settings.prefix(),
            path.trim_end_matches('/')
        ))
        .into_response());
    }
    let html = tokio::fs::read_to_string(app.settings.static_dir.join("index.html")).await?;
    let html = html.replace("__AMALTHEA_BASE__", app.settings.prefix());
    let html = html.replace(
        "__AMALTHEA_IMPRINT_AVAILABLE__",
        if imprint.is_some() { "true" } else { "false" },
    );
    let html = html.replace(
        "<!-- AMALTHEA_IMPRINT -->",
        if path == "/imprint" {
            imprint.as_deref().unwrap_or("")
        } else {
            ""
        },
    );
    Ok((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(html)
        },
    )
        .into_response())
}

async fn me(AxumState(app): AxumState<State>, headers: HeaderMap) -> Result<Json<Value>> {
    let user = auth::user(&app, &headers).await?;
    let _guard = app.mutation.lock().await;
    storage::reconcile_user(&app, &user).await?;
    let (used, reserved) = storage::usage(&app, user.id).await?;
    Ok(Json(
        json!({"user": user, "used_bytes": used, "reserved_bytes": reserved, "chunk_bytes": transfers::CHUNK_BYTES}),
    ))
}
async fn files(
    AxumState(app): AxumState<State>,
    headers: HeaderMap,
) -> Result<Json<Vec<FileRecord>>> {
    let user = auth::user(&app, &headers).await?;
    let _guard = app.mutation.lock().await;
    storage::reconcile_user(&app, &user).await?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM files WHERE user_id = ? ORDER BY name COLLATE NOCASE")
            .bind(user.id)
            .fetch_all(&app.db)
            .await?,
    ))
}
async fn file_meta(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<FileRecord>> {
    let user = auth::user(&app, &headers).await?;
    Ok(Json(storage::owned_file(&app, &user, &id).await?))
}
async fn download(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    let user = auth::user(&app, &headers).await?;
    let record = storage::owned_file(&app, &user, &id).await?;
    transfers::content(&app, &user, record, headers, method, None).await
}
#[derive(Deserialize)]
struct Rename {
    name: String,
}
async fn rename(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Rename>,
) -> Result<StatusCode> {
    let user = auth::user(&app, &headers).await?;
    if !model::valid_name(&input.name) {
        return Err(Error::bad(
            "Invalid filename: use letters, numbers, underscores and dots; no leading dot",
        ));
    }
    let _guard = app.mutation.lock().await;
    let record = storage::owned_file(&app, &user, &id).await?;
    record.require_ready()?;
    if record.name == input.name {
        return Ok(StatusCode::NO_CONTENT);
    }
    let reserved: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM uploads WHERE user_id = ? AND name = ?)")
            .bind(user.id)
            .bind(&input.name)
            .fetch_one(&app.db)
            .await?;
    if reserved {
        return Err(Error::conflict("An unfinished upload reserves that name"));
    }
    let dir = storage::user_dir(&app, &user)?;
    let old = std::ffi::CString::new(dir.join(record.disk_name()?).as_os_str().as_encoded_bytes())
        .map_err(Error::internal)?;
    let new = std::ffi::CString::new(dir.join(&input.name).as_os_str().as_encoded_bytes())
        .map_err(Error::internal)?;
    // SAFETY: both C strings live across the call, and flags/dirfds are constants.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        let e = std::io::Error::last_os_error();
        return Err(if e.kind() == std::io::ErrorKind::AlreadyExists {
            Error::conflict("A file already has that name")
        } else {
            e.into()
        });
    }
    sqlx::query("UPDATE files SET name = ?, disk_stamp = ? WHERE id = ?")
        .bind(&input.name)
        .bind(storage::stamp(
            &tokio::fs::metadata(dir.join(&input.name)).await?,
        ))
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn delete_file(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let user = auth::user(&app, &headers).await?;
    let _upload_guard = app.lock_upload(&id).await;
    let _guard = app.mutation.lock().await;
    let record = storage::owned_file(&app, &user, &id).await?;
    storage::cancel_processing(&app, &id);
    if record.cleanup_upload {
        storage::remove_if_exists(&storage::upload_path(&app, &id)).await?;
    }
    tokio::fs::remove_file(storage::user_dir(&app, &user)?.join(record.disk_name()?)).await?;
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewLink {
    #[serde(default)]
    label: String,
    expires_at: Option<i64>,
}
async fn create_link(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewLink>,
) -> Result<Json<Value>> {
    let user = auth::user(&app, &headers).await?;
    if input.label.len() > 200
        || input
            .expires_at
            .is_some_and(|v| v <= model::now() || v > model::now() + 3650 * 86400)
    {
        return Err(Error::bad(
            "Label must be at most 200 bytes; expiry must be in the next ten years",
        ));
    }
    let _guard = app.mutation.lock().await;
    let file = storage::owned_file(&app, &user, &id).await?;
    file.require_complete()?;
    let secret = model::secret();
    let link_id = model::id();
    sqlx::query("INSERT INTO links (id, file_id, token_hash, label, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?)").bind(&link_id).bind(&id).bind(model::hash(&secret)).bind(input.label).bind(input.expires_at).bind(model::now()).execute(&app.db).await?;
    Ok(Json(
        json!({"id": link_id, "url": format!("{}/s/{}/{}", app.settings.webroot, secret, file.name)}),
    ))
}
async fn links(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let user = auth::user(&app, &headers).await?;
    storage::owned_file(&app, &user, &id)
        .await?
        .require_ready()?;
    let rows = sqlx::query_as::<_, LinkRecord>("SELECT id, label, expires_at, revoked_at, created_at FROM links WHERE file_id = ? ORDER BY created_at DESC").bind(id).fetch_all(&app.db).await?;
    Ok(Json(json!(rows)))
}
async fn check_link_owner(app: &State, user: &User, id: &str) -> Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM links l JOIN files f ON l.file_id = f.id WHERE l.id = ? AND f.user_id = ?)").bind(id).bind(user.id).fetch_one(&app.db).await?;
    if !exists {
        return Err(Error::missing());
    }
    Ok(())
}
async fn revoke_link(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let user = auth::user(&app, &headers).await?;
    check_link_owner(&app, &user, &id).await?;
    sqlx::query("UPDATE links SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?")
        .bind(model::now())
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn logs(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let user = auth::user(&app, &headers).await?;
    check_link_owner(&app, &user, &id).await?;
    let rows: Vec<(String, i64, Option<i64>, Option<i64>)> = sqlx::query_as("SELECT event, at, range_start, range_end FROM access_logs WHERE link_id = ? ORDER BY id DESC LIMIT 200").bind(id).fetch_all(&app.db).await?;
    Ok(Json(json!(rows.into_iter().map(|(event, at, range_start, range_end)| json!({"event": event, "at": at, "range_start": range_start, "range_end": range_end})).collect::<Vec<_>>())))
}
async fn public_file(app: &State, token: &str) -> Result<(User, FileRecord, String)> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT id, file_id FROM links WHERE token_hash = ? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?)").bind(model::hash(token)).bind(model::now()).fetch_optional(&app.db).await?;
    let (link_id, file_id) = row.ok_or_else(Error::missing)?;
    let file: FileRecord = sqlx::query_as("SELECT * FROM files WHERE id = ?")
        .bind(file_id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(Error::missing)?;
    let user = sqlx::query_as("SELECT id, username, quota_bytes FROM users WHERE id = ?")
        .bind(file.user_id)
        .fetch_one(&app.db)
        .await?;
    let file = storage::owned_file(app, &user, &file.id).await?;
    file.require_complete()?;
    Ok((user, file, link_id))
}
async fn public_meta(
    AxumState(app): AxumState<State>,
    Path(token): Path<String>,
    method: Method,
) -> Result<Json<Value>> {
    let (_, file, link) = public_file(&app, &token).await?;
    if method != Method::HEAD {
        sqlx::query("INSERT INTO access_logs (link_id, event, at) VALUES (?, 'view', ?)")
            .bind(link)
            .bind(model::now())
            .execute(&app.db)
            .await?;
    }
    Ok(Json(
        json!({"id": file.id, "name": file.name, "size": file.size, "sha256": file.sha256}),
    ))
}
async fn public_download(
    AxumState(app): AxumState<State>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    let (user, file, link) = public_file(&app, &token).await?;
    transfers::content(&app, &user, file, headers, method, Some(link)).await
}

async fn retry_processing(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let user = auth::user(&app, &headers).await?;
    let _guard = app.mutation.lock().await;
    let file = storage::owned_file(&app, &user, &id).await?;
    if file.status != "failed" {
        return Err(Error::conflict("Only failed processing can be retried"));
    }
    sqlx::query("UPDATE files SET status = 'processing', processing_error = NULL WHERE id = ?")
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
