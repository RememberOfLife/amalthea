use amalthea::{App, configuration::Settings, model, routes};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
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

fn test_config() -> (tempfile::TempDir, PathBuf) {
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
    (dir, config)
}

#[tokio::test]
async fn list_and_revoke_registration_invitations() {
    let (_dir, config) = test_config();
    let empty = success(cli(&config, &["invite", "list"]));
    assert_eq!(empty, "ID\tUSERNAME\tQUOTA\tEXPIRES_AT_UTC\tSTATUS\n");

    let url = success(cli(
        &config,
        &["user", "create", "--any-username", "--quota", "2G"],
    ));
    let token = url.trim().split_once('#').unwrap().1;
    let id = model::hash(token);
    assert_eq!(success(cli(&config, &["invite", "url", &id])), url);
    let other_url = success(cli(&config, &["user", "create", "--any-username"]));
    let other_token = other_url.trim().split_once('#').unwrap().1;
    let other_id = model::hash(other_token);
    let app = App::open(Settings::load(&config).unwrap()).await.unwrap();
    assert!(
        sqlx::query("UPDATE invitations SET token = NULL WHERE token_hash = ?")
            .bind(&id)
            .execute(&app.db)
            .await
            .is_err()
    );
    let expired_token = model::secret();
    let expired_id = model::hash(&expired_token);
    sqlx::query(
        "INSERT INTO invitations (token_hash, token, quota_bytes, expires_at) VALUES (?, ?, 0, ?)",
    )
    .bind(&expired_id)
    .bind(&expired_token)
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
    assert_eq!(
        success(cli(&config, &["invite", "url", &expired_id])),
        format!("http://localhost:8080/register#{expired_token}\n")
    );

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

async fn post(router: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::post(format!("/api/live/register{path}"))
                .header("content-type", "application/json")
                .header("x-amalthea", "1")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn assigned_invitation_enforces_username_and_reservation_lifecycle() {
    let (_dir, config) = test_config();
    assert!(!cli(&config, &["user", "create"]).status.success());
    assert!(
        !cli(&config, &["user", "create", "Alice", "--any-username"])
            .status
            .success()
    );
    let url = success(cli(&config, &["user", "create", "Alice", "--quota", "2G"]));
    let token = url.trim().split_once('#').unwrap().1;
    let open_url = success(cli(&config, &["user", "create", "--any-username"]));
    let open_token = open_url.trim().split_once('#').unwrap().1;
    let app = App::open(Settings::load(&config).unwrap()).await.unwrap();
    let router = routes::router(app.clone());
    assert!(success(cli(&config, &["invite", "list"])).contains("\tAlice\t"));
    assert!(!cli(&config, &["user", "create", "alice"]).status.success());
    assert!(
        !cli(&config, &["user", "create", "../escape"])
            .status
            .success()
    );
    assert_eq!(
        post(&router, "/invitation", json!({"token": token})).await,
        (StatusCode::OK, json!({"username": "Alice"}))
    );
    assert_eq!(
        post(&router, "/invitation", json!({"token": open_token})).await,
        (StatusCode::OK, json!({"username": null}))
    );
    for (token, username, available) in [
        (token, "Alice", true),
        (token, "Bob", false),
        (open_token, "alice", false),
    ] {
        assert_eq!(
            post(
                &router,
                "/check-username",
                json!({"token": token, "username": username})
            )
            .await,
            (StatusCode::OK, json!({"available": available}))
        );
    }
    let credentials = |token: &str, username: &str| {
        json!({
            "token": token, "username": username, "password": "a long correct password"
        })
    };
    for username in ["Bob", "alice"] {
        assert_eq!(
            post(&router, "", credentials(token, username)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    // A generic invitation cannot claim a name assigned to someone else's link.
    assert_eq!(
        post(&router, "", credentials(open_token, "alice")).await.0,
        StatusCode::CONFLICT
    );
    let (status, user) = post(&router, "", credentials(token, "Alice")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["username"], "Alice");
    assert_eq!(user["quota_bytes"], 2147483648_i64);
    assert_eq!(
        post(&router, "", credentials(token, "Alice")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&router, "/invitation", json!({"token": token}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(!cli(&config, &["user", "create", "ALICE"]).status.success());

    // Revocation and expiry both release unused names; failed attempts leave
    // the generic invitation intact for a later successful registration.
    for (name, revoke) in [("Bob", true), ("Carol", false)] {
        let url = success(cli(&config, &["user", "create", name]));
        let token = url.trim().split_once('#').unwrap().1;
        let id = model::hash(token);
        if revoke {
            success(cli(&config, &["invite", "revoke", &id]));
        } else {
            sqlx::query("UPDATE invitations SET expires_at = 0 WHERE token_hash = ?")
                .bind(&id)
                .execute(&app.db)
                .await
                .unwrap();
        }
        assert_eq!(
            post(&router, "/invitation", json!({"token": token}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(&router, "", credentials(token, name)).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(
                &router,
                "/check-username",
                json!({"token": open_token, "username": name})
            )
            .await,
            (StatusCode::OK, json!({"available": true}))
        );
    }
    assert_eq!(
        post(&router, "", credentials(open_token, "Bob")).await.0,
        StatusCode::OK
    );
}
