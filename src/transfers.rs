use crate::{
    State, auth,
    error::{Error, Result},
    model::{self, FileRecord, Upload, User},
    storage,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, State as AxumState},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::os::unix::fs::OpenOptionsExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;

#[derive(Deserialize)]
pub struct NewUpload {
    pub name: String,
    pub size: i64,
    pub sha256: String,
}
pub async fn start(
    AxumState(app): AxumState<State>,
    headers: HeaderMap,
    Json(input): Json<NewUpload>,
) -> Result<Json<Upload>> {
    let user = auth::user(&app, &headers).await?;
    if !model::valid_name(&input.name) {
        return Err(Error::bad(
            "Filenames must use letters, numbers, underscores and dots, and cannot start with a dot (max 128 bytes)",
        ));
    }
    if input.size < 0 || input.size > 9_007_199_254_740_991 || !model::valid_hash(&input.sha256) {
        return Err(Error::bad("Invalid file size or hash"));
    }
    let _guard = app.mutation.lock().await;
    storage::reconcile_user(&app, &user).await?;
    let existing =
        sqlx::query_as::<_, Upload>("SELECT * FROM uploads WHERE user_id = ? AND name = ?")
            .bind(user.id)
            .bind(&input.name)
            .fetch_optional(&app.db)
            .await?;
    if let Some(existing) = existing {
        if existing.size == input.size && existing.sha256 == input.sha256 {
            return Ok(Json(existing));
        }
        return Err(Error::conflict(
            "Another upload already reserves this filename; cancel it first",
        ));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE user_id = ? AND name = ?)")
            .bind(user.id)
            .bind(&input.name)
            .fetch_one(&app.db)
            .await?;
    if exists {
        return Err(Error::conflict(
            "A file already has this name; rename or delete it first",
        ));
    }
    let remaining: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size - offset), 0) FROM uploads")
        .fetch_one(&app.db)
        .await?;
    let available = fs2::available_space(app.settings.data_dir.join("uploads"))?;
    if input.size as u64 > available.saturating_sub(remaining as u64) {
        return Err(Error(
            StatusCode::INSUFFICIENT_STORAGE,
            "Server disk space is insufficient for this upload and existing reservations".into(),
        ));
    }
    let id = model::id();
    let path = storage::upload_path(&app, &id);
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await?;
    file.sync_all().await?;
    std::fs::File::open(app.settings.data_dir.join("uploads"))?.sync_all()?;
    let result = sqlx::query("INSERT INTO uploads (id, user_id, name, size, sha256, offset, updated_at) SELECT ?, id, ?, ?, ?, 0, ? FROM users WHERE id = ? AND quota_bytes - COALESCE((SELECT SUM(size) FROM files WHERE user_id = ?), 0) - COALESCE((SELECT SUM(size) FROM uploads WHERE user_id = ?), 0) >= ?")
        .bind(&id).bind(&input.name).bind(input.size).bind(&input.sha256).bind(model::now()).bind(user.id).bind(user.id).bind(user.id).bind(input.size).execute(&app.db).await;
    match result {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => {
            storage::remove_if_exists(&path).await?;
            return Err(Error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Storage quota exceeded (unfinished uploads also reserve space)".into(),
            ));
        }
        Err(e) => {
            storage::remove_if_exists(&path).await?;
            return Err(e.into());
        }
    }
    Ok(Json(owned_upload(&app, &user, &id).await?))
}
async fn owned_upload(app: &State, user: &User, id: &str) -> Result<Upload> {
    sqlx::query_as("SELECT * FROM uploads WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(user.id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(Error::missing)
}
pub async fn status(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Upload>> {
    let user = auth::user(&app, &headers).await?;
    let upload = owned_upload(&app, &user, &id).await?;
    Ok(Json(upload))
}
pub async fn list(
    AxumState(app): AxumState<State>,
    headers: HeaderMap,
) -> Result<Json<Vec<Upload>>> {
    let user = auth::user(&app, &headers).await?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM uploads WHERE user_id = ? ORDER BY updated_at DESC")
            .bind(user.id)
            .fetch_all(&app.db)
            .await?,
    ))
}
pub async fn chunk(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<Upload>> {
    let user = auth::user(&app, &headers).await?;
    let offset: i64 = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::bad("Upload-Offset is required"))?;
    let hash = headers
        .get("upload-checksum")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::bad("Upload hash is required"))?;
    if bytes.is_empty() || bytes.len() > CHUNK_BYTES {
        return Err(Error::bad("Uploaded part has an invalid size or hash"));
    }
    let checksum_bytes = bytes.clone();
    let checksum = tokio::task::spawn_blocking(move || model::hash(&checksum_bytes)).await?;
    if checksum != hash {
        return Err(Error::bad("Uploaded part has an invalid size or hash"));
    }
    // A disconnected request must not release its lock while filesystem writes
    // are still in flight. The task owns the lock through fsync and offset commit.
    tokio::spawn(async move {
        let _guard = app.lock_upload(&id).await;
        let upload = owned_upload(&app, &user, &id).await?;
        if offset != upload.offset || bytes.len() as i64 > upload.size - offset {
            return Err(Error::conflict(
                "Offset differs from server state; fetch upload status before resuming",
            ));
        }
        let path = storage::upload_path(&app, &id);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() < offset as u64 {
            return Err(Error::conflict(
                "Partial upload is missing bytes; cancel and restart it",
            ));
        }
        let mut file = tokio::fs::File::from_std(file);
        // The committed DB offset is authoritative after an interrupted write.
        file.set_len(offset as u64).await?;
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        sqlx::query("UPDATE uploads SET offset = ?, updated_at = ? WHERE id = ?")
            .bind(offset + bytes.len() as i64)
            .bind(model::now())
            .bind(&id)
            .execute(&app.db)
            .await?;
        Ok(Json(owned_upload(&app, &user, &id).await?))
    })
    .await?
}

pub async fn cancel(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let user = auth::user(&app, &headers).await?;
    let _upload_guard = app.lock_upload(&id).await;
    let _guard = app.mutation.lock().await;
    owned_upload(&app, &user, &id).await?;
    storage::remove_if_exists(&storage::upload_path(&app, &id)).await?;
    sqlx::query("DELETE FROM uploads WHERE id = ?")
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn finish(
    AxumState(app): AxumState<State>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<FileRecord>> {
    let user = auth::user(&app, &headers).await?;
    {
        let _guard = app.mutation.lock().await;
        storage::reconcile_user(&app, &user).await?;
    }
    let _upload_guard = app.lock_upload(&id).await;
    let _guard = app.mutation.lock().await;
    // Reconciliation may have recovered an interrupted publication.
    if let Some(file) = sqlx::query_as("SELECT * FROM files WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(user.id)
        .fetch_optional(&app.db)
        .await?
    {
        return Ok(Json(file));
    }
    let upload = owned_upload(&app, &user, &id).await?;
    if upload.offset != upload.size {
        return Err(Error::conflict("Upload is not complete"));
    }
    let path = storage::upload_path(&app, &id);
    if storage::open_regular(&path)?.metadata()?.len() != upload.size as u64 {
        return Err(Error::conflict(
            "Upload size changed; cancel and upload again",
        ));
    }
    let directory = storage::user_dir(&app, &user)?;
    let destination = directory.join(&upload.name);
    let mut tx = app.db.begin().await?;
    // Lock the DB writer before publishing so a CLI quota update cannot invalidate
    // the quota snapshot between the check and the filesystem mutation.
    sqlx::query("UPDATE users SET quota_bytes = quota_bytes WHERE id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let quota: i64 = sqlx::query_scalar("SELECT quota_bytes FROM users WHERE id = ?")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    let (used, reserved) = storage::usage(&app, user.id).await?;
    if used + reserved > quota {
        return Err(Error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Quota was reduced or disk files were added; free space before completing this upload"
                .into(),
        ));
    }
    // Hard-link publication is atomic and never overwrites an existing file.
    tokio::fs::hard_link(&path, &destination)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::conflict("A file already has this name")
            } else {
                e.into()
            }
        })?;
    // Persist the destination link before removing the durable temporary link.
    std::fs::File::open(&directory)?.sync_all()?;
    storage::remove_if_exists(&path).await?;
    std::fs::File::open(app.settings.data_dir.join("uploads"))?.sync_all()?;
    let disk_stamp = storage::stamp(&tokio::fs::metadata(&destination).await?);
    sqlx::query("INSERT INTO files (id, user_id, name, size, sha256, disk_stamp, created_at, status, expected_sha256) VALUES (?, ?, ?, ?, '', ?, ?, 'processing', ?)")
        .bind(&id).bind(user.id).bind(&upload.name).bind(upload.size).bind(disk_stamp).bind(model::now()).bind(&upload.sha256).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM uploads WHERE id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(storage::owned_file(&app, &user, &id).await?))
}

/// Exactly one HTTP byte range; malformed or unsatisfiable ranges return 416.
pub fn byte_range(value: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        Some((size.saturating_sub(suffix), size - 1))
    } else {
        let start: u64 = start.parse().ok()?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().ok()?.min(size - 1)
        };
        (start <= end && start < size).then_some((start, end))
    }
}

pub async fn content(
    app: &State,
    user: &User,
    record: FileRecord,
    headers: HeaderMap,
    method: Method,
    link_id: Option<String>,
) -> Result<Response> {
    record.require_complete()?;
    let file = storage::open_regular(&storage::user_dir(app, user)?.join(record.disk_name()?))?;
    if storage::stamp(&file.metadata()?) != record.disk_stamp {
        return Err(Error::conflict(
            "File changed on disk; refresh before downloading",
        ));
    }
    let etag = format!("\"{}\"", record.sha256);
    if let Some(condition) = headers.get(header::IF_MATCH)
        && condition != etag.as_str()
        && condition != "*"
    {
        return Err(Error(
            StatusCode::PRECONDITION_FAILED,
            "The file changed; discard the old partial download".into(),
        ));
    }
    let size = record.size as u64;
    let range = if headers
        .get(header::IF_RANGE)
        .is_none_or(|h| h == etag.as_str())
    {
        headers.get(header::RANGE).and_then(|v| v.to_str().ok())
    } else {
        None
    };
    let (status, start, length) = if let Some(range) = range {
        match byte_range(range, size) {
            Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
            None => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                )
                    .into_response());
            }
        }
    } else {
        (StatusCode::OK, 0, size)
    };
    if method != Method::HEAD
        && let Some(id) = link_id
    {
        sqlx::query("INSERT INTO access_logs (link_id, event, at, range_start, range_end) VALUES (?, 'download', ?, ?, ?)").bind(id).bind(model::now()).bind(start as i64).bind((start + length).checked_sub(1).map(|n| n as i64)).execute(&app.db).await?;
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(file);
        file.seek(std::io::SeekFrom::Start(start)).await?;
        Body::from_stream(tokio_util::io::ReaderStream::new(file.take(length)))
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let output = response.headers_mut();
    output.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    output.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    output.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("hex ETag"),
    );
    output.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", record.name))
            .expect("validated filename"),
    );
    output.insert(header::CONTENT_LENGTH, length.into());
    if status == StatusCode::PARTIAL_CONTENT {
        output.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{size}", start + length - 1))
                .expect("numeric range"),
        );
    }
    Ok(response)
}
