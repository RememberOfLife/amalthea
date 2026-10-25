use amalthea::size::{format_size, parse_size};
use amalthea::{App, auth, configuration::Settings, model, storage};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use fs2::FileExt;
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(version, about = "Private, resumable file sharing")]
struct Cli {
    #[arg(long, global = true, default_value = "config.json")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Run the HTTP service (applies pending migrations before listening).
    Server,
    /// List or revoke single-use registration invitations.
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },
    /// Manage accounts and single-use registration invitations.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
}
#[derive(Subcommand)]
enum InviteCommand {
    /// List unused invitations (including expired ones pending cleanup).
    List,
    /// Retrieve an unused invitation's registration URL using its full ID.
    Url { id: String },
    /// Revoke an invitation using its full ID from `invite list`.
    Revoke { id: String },
}
#[derive(Subcommand)]
enum UserCommand {
    /// Print a single-use registration URL. No account is created until it is used.
    Create {
        /// Reserve this username; the recipient chooses only a password.
        #[arg(
            required_unless_present = "any_username",
            conflicts_with = "any_username"
        )]
        username: Option<String>,
        /// Let the recipient choose any available username.
        #[arg(long)]
        any_username: bool,
        #[arg(long, value_parser = parse_size, value_name = "SIZE")]
        quota: Option<i64>,
        #[arg(long, default_value_t = 72)]
        expires_hours: i64,
    },
    /// Show or change a user's quota (raw bytes or e.g. 1.5G, 20M, 3K; binary units).
    Quota {
        username: String,
        #[arg(value_parser = parse_size, value_name = "SIZE")]
        bytes: Option<i64>,
    },
    /// List users, quotas, used bytes and reserved upload bytes.
    List,
    /// Invalidate all login sessions for an account, for example after losing a device.
    RevokeSessions { username: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // New files contain secrets or private data; the service is Linux-only.
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    let settings = Settings::load(&cli.config)?;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&settings.loglevel)?)
        .init();
    let app = App::open(settings.clone()).await?;
    match cli.command {
        Command::Invite { command } => match command {
            InviteCommand::Url { id } => {
                let token: Option<String> =
                    sqlx::query_scalar("SELECT token FROM invitations WHERE token_hash = ?")
                        .bind(id)
                        .fetch_optional(&app.db)
                        .await?;
                let token =
                    token.context("unknown invitation (it may have been used or removed)")?;
                println!("{}/register#{}", settings.webroot, token);
            }
            InviteCommand::List => {
                let invitations: Vec<(String, Option<String>, i64, String, i64)> = sqlx::query_as(
                    "SELECT token_hash, username, quota_bytes, datetime(expires_at, 'unixepoch'), expires_at FROM invitations ORDER BY expires_at, token_hash",
                )
                .fetch_all(&app.db)
                .await?;
                let now = model::now();
                println!("ID\tUSERNAME\tQUOTA\tEXPIRES_AT_UTC\tSTATUS");
                for (id, username, quota, expires, expires_at) in invitations {
                    let status = if expires_at > now {
                        "active"
                    } else {
                        "expired"
                    };
                    println!(
                        "{id}\t{}\t{}\t{expires}\t{status}",
                        username.as_deref().unwrap_or("-"),
                        format_size(quota)
                    );
                }
            }
            InviteCommand::Revoke { id } => {
                let result = sqlx::query("DELETE FROM invitations WHERE token_hash = ?")
                    .bind(&id)
                    .execute(&app.db)
                    .await?;
                if result.rows_affected() == 0 {
                    bail!("unknown invitation: {id} (it may already have been used or removed)");
                }
                println!("Revoked invitation {id}");
            }
        },
        Command::Server => {
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(settings.data_dir.join(".server.lock"))?;
            lock.try_lock_exclusive()
                .context("another server is already using this data directory")?;
            for asset in [
                "index.html",
                "images/favicon.png",
                "images/amalthea.webp",
                "src/main.js",
                "src/username.js",
                "src/api.js",
                "src/errors.js",
                "src/transfers.js",
                "src/browser-storage.js",
                "src/transfer.worker.js",
                "src/transfer-utils.js",
                "src/style.css",
                "vendor/solid.js",
                "vendor/web.js",
                "vendor/html.js",
                "vendor/sha256.js",
                "vendor/bootstrap.min.css",
                "vendor/bootstrap-icons.min.css",
                "vendor/fonts/bootstrap-icons.woff2",
            ] {
                let path = settings.static_dir.join(asset);
                if !path.is_file() {
                    bail!(
                        "frontend asset is missing: {} (check static_dir)",
                        path.display()
                    );
                }
            }
            let listener = tokio::net::TcpListener::bind(settings.listen)
                .await
                .context("cannot bind configured listen address")?;
            storage::maintenance(&app)
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            let processing = amalthea::processing::start(app.clone());
            let background_app = app.clone();
            let maintenance = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(
                        background_app.settings.reconcile_seconds,
                    ))
                    .await;
                    if let Err(e) = storage::maintenance(&background_app).await {
                        tracing::error!(error = %e.1, "maintenance failed");
                    }
                }
            });
            tracing::info!(address = %settings.listen, "Amalthea listening");
            axum::serve(listener, amalthea::routes::router(app))
                .with_graceful_shutdown(async {
                    let mut terminate =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("SIGTERM handler");
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
                })
                .await?;
            maintenance.abort();
            processing.abort();
            drop(lock);
        }
        Command::User { command } => match command {
            UserCommand::RevokeSessions { username } => {
                let id: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
                    .bind(&username)
                    .fetch_optional(&app.db)
                    .await?;
                let id = id.context("unknown user")?;
                let result = sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                    .bind(id)
                    .execute(&app.db)
                    .await?;
                println!(
                    "Revoked {} session(s) for {username}",
                    result.rows_affected()
                );
            }
            UserCommand::Create {
                username,
                any_username: _,
                quota,
                expires_hours,
            } => {
                let quota = quota.unwrap_or(settings.default_quota_bytes);
                validate_quota(quota)?;
                if !(1..=87600).contains(&expires_hours) {
                    bail!("expires-hours must be between 1 and 87600");
                }
                // Hold the SQLite writer lock through validation and insertion,
                // including when the CLI runs alongside the server.
                let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
                if let Some(username) = &username {
                    if !model::valid_username(username) {
                        bail!(
                            "username must contain 1-64 ASCII letters, numbers, _, ., -, @ or +; no leading dot"
                        );
                    }
                    if !auth::username_available(&app, &mut tx, username, "")
                        .await
                        .map_err(|e| anyhow::anyhow!(e.1))?
                    {
                        bail!(
                            "username is already taken, assigned to an invitation, or has an existing data directory"
                        );
                    }
                }
                let token = model::secret();
                sqlx::query("INSERT INTO invitations (token_hash, token, username, quota_bytes, expires_at) VALUES (?, ?, ?, ?, ?)").bind(model::hash(&token)).bind(&token).bind(username).bind(quota).bind(model::now() + expires_hours * 3600).execute(&mut *tx).await?;
                tx.commit().await?;
                println!("{}/register#{}", settings.webroot, token);
            }
            UserCommand::Quota { username, bytes } => {
                if let Some(bytes) = bytes {
                    validate_quota(bytes)?;
                    let result = sqlx::query("UPDATE users SET quota_bytes = ? WHERE username = ?")
                        .bind(bytes)
                        .bind(&username)
                        .execute(&app.db)
                        .await?;
                    if result.rows_affected() == 0 {
                        bail!("unknown user: {username}");
                    }
                }
                let quota: Option<i64> =
                    sqlx::query_scalar("SELECT quota_bytes FROM users WHERE username = ?")
                        .bind(&username)
                        .fetch_optional(&app.db)
                        .await?;
                println!(
                    "{}\t{}",
                    username,
                    format_size(quota.context("unknown user")?)
                );
            }
            UserCommand::List => {
                let users = sqlx::query_as::<_, model::User>(
                    "SELECT id, username, quota_bytes FROM users ORDER BY username",
                )
                .fetch_all(&app.db)
                .await?;
                println!("USERNAME\tQUOTA_BYTES\tUSED_BYTES\tRESERVED_BYTES");
                for user in users {
                    let (used, reserved) = storage::usage(&app, user.id)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.1))?;
                    println!(
                        "{}\t{}\t{}\t{}",
                        user.username,
                        format_size(user.quota_bytes),
                        format_size(used),
                        format_size(reserved)
                    );
                }
            }
        },
    }
    Ok(())
}
fn validate_quota(bytes: i64) -> anyhow::Result<()> {
    if !(0..=9_007_199_254_740_991).contains(&bytes) {
        bail!("quota must be between 0 and 9007199254740991 bytes");
    }
    Ok(())
}
