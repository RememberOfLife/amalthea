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
    tokio::task::spawn_blocking(move || {
        let mut file = open_regular(&path)?;
        let before = file.metadata()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
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
                meta.is_file()
                    && !meta.file_type().is_symlink()
                    && stamp(&meta) == record.disk_stamp
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if !unchanged {
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
        let (size, hash, disk_stamp) = digest_file(entry.path()).await?;
        // Recover a commit interrupted after publishing the file but before the DB transaction.
        let pending: Option<String> = sqlx::query_scalar("SELECT id FROM uploads WHERE user_id = ? AND name = ? AND size = ? AND sha256 = ? AND offset = size")
            .bind(user.id).bind(&name).bind(size).bind(&hash).fetch_optional(&app.db).await?;
        let file_id = pending.clone().unwrap_or_else(model::id);
        let mut tx = app.db.begin().await?;
        sqlx::query("INSERT INTO files (id, user_id, name, size, sha256, disk_stamp, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&file_id).bind(user.id).bind(&name).bind(size).bind(&hash).bind(disk_stamp).bind(model::now()).execute(&mut *tx).await?;
        if let Some(id) = &pending {
            sqlx::query("DELETE FROM uploads WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if let Some(id) = pending {
            remove_if_exists(&upload_path(app, &id)).await?;
            sqlx::query("UPDATE files SET disk_stamp = ? WHERE id = ?")
                .bind(stamp(&tokio::fs::metadata(entry.path()).await?))
                .bind(id)
                .execute(&app.db)
                .await?;
        }
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
    let _guard = app.mutation.lock().await;
    let users = sqlx::query_as::<_, User>("SELECT id, username, quota_bytes FROM users")
        .fetch_all(&app.db)
        .await?;
    for user in users {
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
        remove_if_exists(&upload_path(app, &id)).await?;
        sqlx::query("DELETE FROM uploads WHERE id = ?")
            .bind(id)
            .execute(&app.db)
            .await?;
    }
    let mut entries = tokio::fs::read_dir(app.settings.data_dir.join("uploads")).await?;
    while let Some(entry) = entries.next_entry().await? {
        let id = entry.file_name().to_string_lossy().into_owned();
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM uploads WHERE id = ?)")
            .bind(id)
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
    if stamp(&file.metadata()?) != record.disk_stamp {
        return Err(Error::conflict(
            "File changed on disk; refresh the file list",
        ));
    }
    Ok(record)
}
