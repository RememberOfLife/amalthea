use crate::{
    State,
    error::{Error, Result},
    model::{self, FileRecord, User},
};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub fn user_dir(app: &State, user: &User) -> Result<PathBuf> {
    if !model::valid_username(&user.username) {
        return Err(Error::conflict("Invalid stored username"));
    }
    let path = app.settings.data_dir.join("users").join(&user.username);
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::conflict(
            "User data directory must be a real directory",
        ));
    }
    Ok(path)
}
pub fn upload_path(app: &State, id: &str) -> PathBuf {
    app.settings.data_dir.join("uploads").join(id)
}
pub fn stamp(meta: &std::fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    )
}
// Unlinking an interrupted publication's temporary hard link changes ctime.
// During recovery only, tolerate that change so a crash between unlink and DB
// commit can resume. Device, inode, size and mtime must still match.
pub fn matches_record(meta: &std::fs::Metadata, record: &FileRecord) -> bool {
    let current = stamp(meta);
    if record.cleanup_upload {
        current
            .split(':')
            .take(5)
            .eq(record.disk_stamp.split(':').take(5))
    } else {
        current == record.disk_stamp
    }
}
pub fn open_regular(path: &Path) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::missing()
            } else {
                Error::internal(e)
            }
        })?;
    if !f.metadata()?.is_file() {
        return Err(Error::missing());
    }
    Ok(f)
}
pub async fn digest_file(path: PathBuf) -> Result<(i64, String, String)> {
    digest_cancellable(path, tokio_util::sync::CancellationToken::new()).await
}
pub async fn digest_cancellable(
    path: PathBuf,
    token: tokio_util::sync::CancellationToken,
) -> Result<(i64, String, String)> {
    tokio::task::spawn_blocking(move || {
        let mut file = open_regular(&path)?;
        let before = file.metadata()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            if token.is_cancelled() {
                return Err(Error::conflict("Processing cancelled"));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        if stamp(&before) != stamp(&file.metadata()?) {
            return Err(Error::conflict(
                "File changed while being read; please retry",
            ));
        }
        Ok((
            before.len() as i64,
            format!("{:x}", hasher.finalize()),
            stamp(&before),
        ))
    })
    .await?
}
pub async fn usage(app: &State, user_id: i64) -> Result<(i64, i64)> {
    let used = sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM files WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(&app.db)
        .await?;
    let reserved =
        sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM uploads WHERE user_id = ?")
            .bind(user_id)
            .fetch_one(&app.db)
            .await?;
    Ok((used, reserved))
}

/// Caller holds the mutation lock. Subdirectories and symlinks are never imported. Invalid filenames remain incomplete.
/// Replaced content gets a new identity: old sharing links cannot accidentally expose new bytes.
pub async fn reconcile_user(app: &State, user: &User) -> Result<()> {
    let dir = user_dir(app, user)?;
    let records = sqlx::query_as::<_, FileRecord>("SELECT * FROM files WHERE user_id = ?")
        .bind(user.id)
        .fetch_all(&app.db)
        .await?;
    for record in records {
        let path = dir.join(record.disk_name()?);
        let unchanged = match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) => {
                meta.is_file() && !meta.file_type().is_symlink() && matches_record(&meta, &record)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if !unchanged {
            cancel_processing(app, &record.id);
            sqlx::query("DELETE FROM files WHERE id = ?")
                .bind(&record.id)
                .execute(&app.db)
                .await?;
        }
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = model::filename_key(&entry.file_name());
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE user_id = ? AND name = ?)")
                .bind(user.id)
                .bind(&name)
                .fetch_one(&app.db)
                .await?;
        if exists {
            continue;
        }
        let meta = tokio::fs::symlink_metadata(entry.path()).await?;
        if !meta.is_file() {
            continue;
        }
        let size = meta.len() as i64;
        // Recover publication using the durable upload reservation. Verification still
        // happens in the worker, so an unverified file can never be downloaded.
        let pending: Option<(String, String)> = sqlx::query_as("SELECT id, sha256 FROM uploads WHERE user_id = ? AND name = ? AND size = ? AND offset = size")
            .bind(user.id).bind(&name).bind(size).fetch_optional(&app.db).await?;
        let _upload_guard = match &pending {
            Some((id, _)) => match app.try_lock_upload(id) {
                Some(guard) => Some(guard),
                None => continue,
            },
            None => None,
        };
        let pending = match pending {
            Some((id, hash)) => match tokio::fs::symlink_metadata(upload_path(app, &id)).await {
                Ok(upload) if upload.dev() == meta.dev() && upload.ino() == meta.ino() => {
                    Some((id, hash))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some((id, hash)),
                Ok(_) => None,
                Err(e) => return Err(e.into()),
            },
            None => None,
        };
        let file_id = pending
            .as_ref()
            .map(|p| p.0.clone())
            .unwrap_or_else(model::id);
        let disk_stamp = stamp(&tokio::fs::symlink_metadata(entry.path()).await?);
        let mut tx = app.db.begin().await?;
        sqlx::query("INSERT INTO files (id, user_id, name, size, sha256, disk_stamp, created_at, status, expected_sha256, cleanup_upload) VALUES (?, ?, ?, ?, '', ?, ?, 'processing', ?, ?)")
            .bind(&file_id).bind(user.id).bind(&name).bind(size).bind(disk_stamp).bind(model::now())
            .bind(pending.as_ref().map(|p| &p.1)).bind(pending.is_some()).execute(&mut *tx).await?;
        if let Some((id, _)) = pending {
            sqlx::query("DELETE FROM uploads WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

pub async fn remove_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub async fn maintenance(app: &State) -> Result<()> {
    let users = sqlx::query_as::<_, User>("SELECT id, username, quota_bytes FROM users")
        .fetch_all(&app.db)
        .await?;
    for user in users {
        let _guard = app.mutation.lock().await;
        if let Err(error) = reconcile_user(app, &user).await {
            tracing::warn!(user_id = user.id, error = %error.1, "reconciliation failed");
        }
    }
    let cutoff = model::now() - app.settings.upload_expiry_days * 86400;
    let stale: Vec<String> = sqlx::query_scalar("SELECT id FROM uploads WHERE updated_at < ?")
        .bind(cutoff)
        .fetch_all(&app.db)
        .await?;
    for id in stale {
        let Some(_upload_guard) = app.try_lock_upload(&id) else {
            continue;
        };
        let _guard = app.mutation.lock().await;
        // A chunk may have committed since the initial expiry scan.
        let expired: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM uploads WHERE id = ? AND updated_at < ?)",
        )
        .bind(&id)
        .bind(cutoff)
        .fetch_one(&app.db)
        .await?;
        if !expired {
            continue;
        }
        remove_if_exists(&upload_path(app, &id)).await?;
        sqlx::query("DELETE FROM uploads WHERE id = ?")
            .bind(id)
            .execute(&app.db)
            .await?;
    }
    let mut entries = tokio::fs::read_dir(app.settings.data_dir.join("uploads")).await?;
    while let Some(entry) = entries.next_entry().await? {
        let id = entry.file_name().to_string_lossy().into_owned();
        let Some(_upload_guard) = app.try_lock_upload(&id) else {
            continue;
        };
        let _guard = app.mutation.lock().await;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM uploads WHERE id = ?) OR EXISTS(SELECT 1 FROM files WHERE id = ? AND cleanup_upload = 1)")
            .bind(&id).bind(&id)
            .fetch_one(&app.db)
            .await?;
        if !exists && entry.file_type().await?.is_file() {
            remove_if_exists(&entry.path()).await?;
        }
    }
    sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(model::now())
        .execute(&app.db)
        .await?;
    sqlx::query("DELETE FROM invitations WHERE expires_at <= ?")
        .bind(model::now())
        .execute(&app.db)
        .await?;
    sqlx::query("DELETE FROM access_logs WHERE at < ?")
        .bind(model::now() - app.settings.access_log_days * 86400)
        .execute(&app.db)
        .await?;
    Ok(())
}

pub async fn owned_file(app: &State, user: &User, id: &str) -> Result<FileRecord> {
    let record =
        sqlx::query_as::<_, FileRecord>("SELECT * FROM files WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user.id)
            .fetch_optional(&app.db)
            .await?
            .ok_or_else(Error::missing)?;
    let path = user_dir(app, user)?.join(record.disk_name()?);
    let file = open_regular(&path)?;
    if !matches_record(&file.metadata()?, &record) {
        return Err(Error::conflict(
            "File changed on disk; refresh the file list",
        ));
    }
    Ok(record)
}

pub fn cancel_processing(app: &State, id: &str) {
    if let Some(token) = app.processing.lock().unwrap().get(id) {
        token.cancel();
    }
}
