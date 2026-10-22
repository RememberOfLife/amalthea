//! SQLite-specific setup lives here. Schema changes are ordered, embedded migrations;
//! route queries use bound parameters and ordinary relational tables.
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, time::Duration};

pub async fn open(directory: &Path) -> anyhow::Result<SqlitePool> {
    tokio::fs::create_dir_all(directory).await?;
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.join("amalthea.sqlite3"))
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full)
                .busy_timeout(Duration::from_secs(30)),
        )
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
