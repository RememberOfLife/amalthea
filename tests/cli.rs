use amalthea::{App, configuration::Settings, model, routes};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use std::{
    path::Path,
    process::{Command, Output},
};
use tower::ServiceExt;

fn cli(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_amalthea"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn list_and_revoke_registration_invitations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(
        &config,
        json!({
            "data_dir": "data",
            "static_dir": Path::new(env!("CARGO_MANIFEST_DIR")).join("res/static"),
            "loglevel": "error"
        })
        .to_string(),
    )
    .unwrap();
    let empty = success(cli(&config, &["invite", "list"]));
    assert_eq!(empty, "ID\tQUOTA\tEXPIRES_AT_UTC\tSTATUS\n");

    let url = success(cli(&config, &["user", "create", "--quota", "2G"]));
    let token = url.trim().split_once('#').unwrap().1;
    let id = model::hash(token);
    assert_eq!(success(cli(&config, &["invite", "url", &id])), url);
    let other_url = success(cli(&config, &["user", "create"]));
    let other_token = other_url.trim().split_once('#').unwrap().1;
    let other_id = model::hash(other_token);
    let app = App::open(Settings::load(&config).unwrap()).await.unwrap();
    let expired_id = model::hash("expired invitation");
    sqlx::query("INSERT INTO invitations (token_hash, quota_bytes, expires_at) VALUES (?, 0, ?)")
        .bind(&expired_id)
        .bind(model::now() - 60)
        .execute(&app.db)
        .await
        .unwrap();

    let listing = success(cli(&config, &["invite", "list"]));
    let row = listing.lines().find(|line| line.starts_with(&id)).unwrap();
    assert!(row.contains("\t2147483648 (2 GiB)\t"), "{row}");
    assert!(row.ends_with("\tactive"));
    assert!(
        listing
            .lines()
            .any(|line| line.starts_with(&expired_id) && line.ends_with("\texpired"))
    );
    assert!(!listing.contains(token));
    let legacy = cli(&config, &["invite", "url", &expired_id]);
    assert!(!legacy.status.success());
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("predates saved links"));

    success(cli(&config, &["invite", "revoke", &id]));
    let listing = success(cli(&config, &["invite", "list"]));
    assert!(!listing.contains(&id));
    assert!(listing.contains(&other_id));
    let missing = cli(&config, &["invite", "revoke", &id]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("unknown invitation"));
    assert!(!cli(&config, &["invite", "url", &id]).status.success());

    let router = routes::router(app);
    for (token, expected) in [
        (token, StatusCode::BAD_REQUEST),
        (other_token, StatusCode::OK),
    ] {
        let response = router.clone().oneshot(
            Request::post("/api/live/register")
                .header("content-type", "application/json")
                .header("x-amalthea", "1")
                .body(Body::from(json!({
                    "token": token, "username": "Alice", "password": "a long correct password"
                }).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), expected);
    }
    success(cli(&config, &["invite", "revoke", &expired_id]));
    assert!(!cli(&config, &["invite", "url", &other_id]).status.success());
    assert_eq!(success(cli(&config, &["invite", "list"])), empty);
}
