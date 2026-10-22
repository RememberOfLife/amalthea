//! File rows are the durable queue. Only the server starts this dispatcher.
//! A running row remains "processing", so interrupted jobs resume after restart.
use crate::{
    State,
    error::Result,
    model::{FileRecord, User},
    storage,
};
use std::collections::HashSet;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub async fn next(app: &State, after: i64, active: &HashSet<String>) -> Result<Option<FileRecord>> {
    let mut query =
        sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT * FROM files WHERE status = 'processing'");
    if !active.is_empty() {
        query.push(" AND id NOT IN (");
        let mut ids = query.separated(", ");
        for id in active {
            ids.push_bind(id);
        }
        query.push(")");
    }
    query
        .push(" ORDER BY (user_id <= ")
        .push_bind(after)
        .push("), user_id, created_at, rowid LIMIT 1");
    Ok(query.build_query_as().fetch_optional(&app.db).await?)
}

pub fn start(app: State) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut workers = JoinSet::new();
        let mut active = HashSet::new();
        let mut task_files = std::collections::HashMap::new();
        let mut after = 0;
        loop {
            while workers.len() < app.settings.processing_workers {
                // Serialize dispatch with deletion, but never hold this lock while hashing.
                let guard = app.mutation.lock().await;
                let file = match next(&app, after, &active).await {
                    Ok(Some(file)) => file,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(error = %error.1, "processing dispatch failed");
                        break;
                    }
                };
                after = file.user_id;
                active.insert(file.id.clone());
                let token = CancellationToken::new();
                app.processing
                    .lock()
                    .unwrap()
                    .insert(file.id.clone(), token.clone());
                drop(guard);
                let app = app.clone();
                let id = file.id.clone();
                let task = workers.spawn(async move {
                    let _cancel_on_exit = token.clone().drop_guard();
                    if let Err(error) = process(&app, &file, token).await {
                        tracing::error!(file_id = %file.id, error = %error.1, "processing failed");
                        let _ = sqlx::query("UPDATE files SET status = 'failed', processing_error = 'Processing failed; please retry' WHERE id = ? AND status = 'processing'")
                            .bind(&file.id).execute(&app.db).await;
                    }
                });
                task_files.insert(task.id(), id);
            }
            tokio::select! {
                Some(result) = workers.join_next_with_id(), if !workers.is_empty() => {
                    let (task_id, failed) = match result {
                        Ok((id, ())) => (id, false),
                        Err(error) => {
                            tracing::error!(%error, "processing worker panicked");
                            (error.id(), true)
                        }
                    };
                    if let Some(id) = task_files.remove(&task_id) {
                        active.remove(&id);
                        app.processing.lock().unwrap().remove(&id);
                        if failed {
                            let _ = sqlx::query("UPDATE files SET status = 'failed', processing_error = 'Processing interrupted; please retry' WHERE id = ? AND status = 'processing'")
                                .bind(id).execute(&app.db).await;
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
        }
    })
}

pub async fn process(app: &State, file: &FileRecord, token: CancellationToken) -> Result<()> {
    let user =
        sqlx::query_as::<_, User>("SELECT id, username, quota_bytes FROM users WHERE id = ?")
            .bind(file.user_id)
            .fetch_one(&app.db)
            .await?;
    let path = storage::user_dir(app, &user)?.join(file.disk_name()?);
    let before = storage::open_regular(&path).and_then(|f| Ok(f.metadata()?));
    let expected_stamp = before
        .as_ref()
        .ok()
        .filter(|meta| storage::matches_record(meta, file))
        .map(storage::stamp);
    let result = storage::digest_cancellable(path.clone(), token.clone()).await;
    let _upload_guard = if file.cleanup_upload {
        Some(app.lock_upload(&file.id).await)
    } else {
        None
    };
    let _guard = app.mutation.lock().await;
    if token.is_cancelled() {
        return Ok(());
    }
    let outcome: std::result::Result<(), &str> = match result {
        Ok((size, hash, stamp))
            if size == file.size
                && Some(&stamp) == expected_stamp.as_ref()
                && std::fs::symlink_metadata(&path)
                    .is_ok_and(|meta| storage::stamp(&meta) == stamp) =>
        {
            if file
                .expected_sha256
                .as_ref()
                .is_some_and(|expected| expected != &hash)
            {
                Err("Complete file checksum does not match; delete and upload again")
            } else {
                // Recovery cleanup is part of this same bounded, fair job. Keep
                // its durable flag until unlink + directory sync have completed.
                if file.cleanup_upload {
                    storage::remove_if_exists(&storage::upload_path(app, &file.id)).await?;
                    sync_directory(app.settings.data_dir.join("uploads")).await?;
                }
                let disk_stamp = storage::stamp(&tokio::fs::symlink_metadata(&path).await?);
                sqlx::query("UPDATE files SET sha256 = ?, disk_stamp = ?, cleanup_upload = 0, status = 'ready', processing_error = NULL WHERE id = ? AND disk_stamp = ? AND status = 'processing'")
                    .bind(hash).bind(disk_stamp).bind(&file.id).bind(&file.disk_stamp).execute(&app.db).await?;
                return Ok(());
            }
        }
        Ok(_) => Err("File changed while processing; refresh the file list"),
        Err(_) => Err("Could not read file; check the file and retry"),
    };
    sqlx::query("UPDATE files SET status = 'failed', processing_error = ? WHERE id = ? AND status = 'processing'")
        .bind(outcome.unwrap_err()).bind(&file.id).execute(&app.db).await?;
    Ok(())
}

async fn sync_directory(path: std::path::PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all()).await??;
    Ok(())
}
