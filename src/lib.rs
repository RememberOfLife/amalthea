pub mod auth;
pub mod configuration;
pub mod db;
pub mod error;
pub mod model;
pub mod processing;
pub mod routes;
pub mod size;
pub mod storage;
pub mod transfers;

use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, Semaphore};

pub struct App {
    pub settings: configuration::Settings,
    pub db: sqlx::SqlitePool,
    // Serializes metadata/filesystem mutations, including quota reservations and reconciliation.
    // Chunk I/O and streaming downloads never hold this lock. When both locks
    // are needed, acquire the upload lock BEFORE mutation. Reconciliation
    // already holds mutation and may only TRY an upload lock (skip busy uploads).
    pub mutation: Mutex<()>,
    upload_locks: std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    pub processing: std::sync::Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    pub password_jobs: Semaphore,
    pub login_attempts: Mutex<HashMap<String, (i64, u32)>>,
}
pub type State = Arc<App>;

impl App {
    /// Serializes writes/cancellation/publication for one temporary upload only.
    fn upload_lock(&self, id: &str) -> Arc<Mutex<()>> {
        {
            let mut locks = self.upload_locks.lock().unwrap();
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(id).and_then(std::sync::Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(id.to_owned(), Arc::downgrade(&lock));
                    lock
                }
            }
        }
    }

    pub async fn lock_upload(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.upload_lock(id).lock_owned().await
    }

    pub fn try_lock_upload(&self, id: &str) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        self.upload_lock(id).try_lock_owned().ok()
    }

    pub async fn open(settings: configuration::Settings) -> anyhow::Result<State> {
        for name in ["users", "uploads"] {
            let path = settings.data_dir.join(name);
            tokio::fs::create_dir_all(&path).await?;
            if tokio::fs::symlink_metadata(&path)
                .await?
                .file_type()
                .is_symlink()
            {
                anyhow::bail!(
                    "storage subdirectory must not be a symlink: {}",
                    path.display()
                );
            }
        }
        let data = tokio::fs::canonicalize(&settings.data_dir).await?;
        let public = tokio::fs::canonicalize(&settings.static_dir).await?;
        if data.starts_with(&public) {
            anyhow::bail!("data_dir must not be inside static_dir (private data would be public)");
        }
        let db = db::open(&settings.data_dir).await?;
        Ok(Arc::new(Self {
            settings,
            db,
            mutation: Mutex::new(()),
            upload_locks: std::sync::Mutex::new(HashMap::new()),
            processing: std::sync::Mutex::new(HashMap::new()),
            password_jobs: Semaphore::new(2),
            login_attempts: Mutex::new(HashMap::new()),
        }))
    }
}
