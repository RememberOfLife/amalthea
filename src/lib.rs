pub mod auth;
pub mod configuration;
pub mod db;
pub mod error;
pub mod model;
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
    // Streaming downloads never hold this lock.
    pub mutation: Mutex<()>,
    pub password_jobs: Semaphore,
    pub login_attempts: Mutex<HashMap<String, (i64, u32)>>,
}
pub type State = Arc<App>;

impl App {
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
            password_jobs: Semaphore::new(2),
            login_attempts: Mutex::new(HashMap::new()),
        }))
    }
}
