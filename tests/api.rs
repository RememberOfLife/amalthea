use amalthea::{App, State, configuration::Settings, model, routes, storage};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Harness {
    _dir: TempDir,
    app: State,
    router: Router,
}
impl Harness {
    async fn new() -> Self {
        Self::with_prefix("").await
    }
    async fn with_prefix(prefix: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-data");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let mut settings = Settings {
            data_dir: dir.path().join("data"),
            imprint: Some(dir.path().join("imprint.html")),
            static_dir: std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("res/static"),
            webroot: format!("https://example.test{prefix}"),
            ..Settings::default()
        };
        settings.validate().unwrap();
        let app = App::open(settings).await.unwrap();
        Self {
            _dir: dir,
            router: routes::router(app.clone()),
            app,
        }
    }
    async fn seed(&self, username: &str, quota: i64) -> String {
        let id = sqlx::query("INSERT INTO users (username,password_hash,quota_bytes,created_at) VALUES (?, 'unused', ?, ?)").bind(username).bind(quota).bind(model::now()).execute(&self.app.db).await.unwrap().last_insert_rowid();
        std::fs::create_dir(self.app.settings.data_dir.join("users").join(username)).unwrap();
        let secret = model::secret();
        sqlx::query("INSERT INTO sessions (token_hash,user_id,expires_at) VALUES (?,?,?)")
            .bind(model::hash(&secret))
            .bind(id)
            .bind(model::now() + 1000)
            .execute(&self.app.db)
            .await
            .unwrap();
        format!("amalthea_session={secret}")
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        cookie: &str,
        body: Vec<u8>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.app.settings.prefix()))
            .header("x-amalthea", "1");
        if !cookie.is_empty() {
            req = req.header("cookie", cookie);
        }
        for (key, value) in headers {
            req = req.header(*key, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }
    async fn json(
        &self,
        method: &str,
        path: &str,
        cookie: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, _, bytes) = self
            .request(
                method,
                path,
                cookie,
                serde_json::to_vec(&body).unwrap(),
                &[("content-type", "application/json")],
            )
            .await;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }
    async fn start(&self, cookie: &str, name: &str, bytes: &[u8]) -> String {
        let (status, body) = self
            .json(
                "POST",
                "/api/live/uploads",
                cookie,
                json!({"name":name,"size":bytes.len(),"sha256":model::hash(bytes)}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }
    async fn chunk(&self, cookie: &str, id: &str, offset: u64, bytes: &[u8]) -> StatusCode {
        self.request(
            "PUT",
            &format!("/api/live/uploads/{id}"),
            cookie,
            bytes.to_vec(),
            &[
                ("upload-offset", &offset.to_string()),
                ("upload-checksum", &model::hash(bytes)),
            ],
        )
        .await
        .0
    }
    async fn finish(&self, cookie: &str, id: &str) -> (StatusCode, Value) {
        self.json(
            "POST",
            &format!("/api/live/uploads/{id}/finish"),
            cookie,
            Value::Null,
        )
        .await
    }
    async fn drain(&self) {
        while let Some(file) = amalthea::processing::next(&self.app, 0, &Default::default())
            .await
            .unwrap()
        {
            amalthea::processing::process(
                &self.app,
                &file,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        }
    }
    async fn file(&self, cookie: &str, name: &str, bytes: &[u8]) -> String {
        let id = self.start(cookie, name, bytes).await;
        if !bytes.is_empty() {
            assert_eq!(self.chunk(cookie, &id, 0, bytes).await, StatusCode::OK);
        }
        let (status, body) = self.finish(cookie, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        self.drain().await;
        id
    }
    async fn link(&self, cookie: &str, id: &str) -> (String, String) {
        self.drain().await;
        let (status, body) = self
            .json(
                "POST",
                &format!("/api/live/files/{id}/links"),
                cookie,
                json!({"label":"recipient","expires_at":model::now()+3600}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = body["url"]
            .as_str()
            .unwrap()
            .split("/s/")
            .nth(1)
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        (body["id"].as_str().unwrap().to_owned(), token)
    }
}

#[tokio::test]
async fn registration_username_availability() {
    let h = Harness::new().await;
    h.seed("Taken", 100).await;
    std::fs::create_dir(h.app.settings.data_dir.join("users/orphan")).unwrap();
    let token = model::secret();
    sqlx::query("INSERT INTO invitations VALUES (?,?,?)")
        .bind(model::hash(&token))
        .bind(100)
        .bind(model::now() + 1000)
        .execute(&h.app.db)
        .await
        .unwrap();
    for (username, available) in [
        ("new_user", true),
        ("taken", false),
        ("LOGIN", false),
        ("bad-id", false),
        ("../escape", false),
        ("", false),
        ("orphan", false),
    ] {
        let (status, body) = h
            .json(
                "POST",
                "/api/live/register/check-username",
                "",
                json!({"token": token, "username": username}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["available"], available, "{username}");
    }
    sqlx::query("UPDATE invitations SET expires_at = 0")
        .execute(&h.app.db)
        .await
        .unwrap();
    for invalid_token in [&token, "invalid"] {
        let (status, _) = h
            .json(
                "POST",
                "/api/live/register/check-username",
                "",
                json!({"token": invalid_token, "username": "new_user"}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn invitation_registration_login_and_logout() {
    let h = Harness::with_prefix("/files").await;
    let invite = model::secret();
    sqlx::query("INSERT INTO invitations VALUES (?,?,?)")
        .bind(model::hash(&invite))
        .bind(12345)
        .bind(model::now() + 60)
        .execute(&h.app.db)
        .await
        .unwrap();
    let input = json!({"username":"Alice","password":"a long correct password","token":invite});
    let (status, headers, body) = h
        .request(
            "POST",
            "/api/live/register",
            "",
            serde_json::to_vec(&input).unwrap(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let cookie = headers["set-cookie"].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("Secure")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/files/")
    );
    assert_eq!(
        h.json("POST", "/api/live/register", "", input).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.json(
            "POST",
            "/api/live/login",
            "",
            json!({"username":"aLiCe","password":"wrong"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, _) = h
        .request(
            "POST",
            "/api/live/login",
            "",
            serde_json::to_vec(&json!({"username":"alice","password":"a long correct password"}))
                .unwrap(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let session = cookie.split(';').next().unwrap();
    let (_, me) = h.json("GET", "/api/live/me", session, Value::Null).await;
    assert_eq!(me["user"]["quota_bytes"], 12345);
    assert_eq!(
        h.json("POST", "/api/live/logout", session, Value::Null)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        h.json("GET", "/api/live/me", session, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let stored: String = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert!(stored.starts_with("$argon2id$"));
}

#[tokio::test]
async fn accounts_are_isolated_and_names_cannot_escape() {
    let h = Harness::new().await;
    let a = h.seed("Alice", 1000).await;
    let b = h.seed("Bob", 1000).await;
    let id = h.file(&a, "private.txt", b"hello").await;
    for path in [
        format!("/api/live/files/{id}"),
        format!("/api/live/files/{id}/content"),
        format!("/api/live/files/{id}/links"),
    ] {
        assert_eq!(
            h.json("GET", &path, &b, Value::Null).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.json("GET", &path, "", Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        h.json("DELETE", &format!("/api/live/files/{id}"), &b, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json(
            "PATCH",
            &format!("/api/live/files/{id}"),
            &b,
            json!({"name":"stolen"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for name in [
        "../secret",
        ".hidden",
        "a/b",
        "a\\b",
        "",
        "a space",
        "x\".txt",
    ] {
        assert_eq!(
            h.json(
                "POST",
                "/api/live/uploads",
                &a,
                json!({"name":name,"size":1,"sha256":model::hash("a")})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let upload = h.start(&a, "pending", b"unfinished").await;
    assert_eq!(
        h.chunk(&b, &upload, 0, b"unfinished").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/uploads/{upload}"),
            &b,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM links")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(links, 0);
}

#[tokio::test]
async fn quotas_reserve_atomically_and_release_on_cancel() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 10).await;
    let input = |name| json!({"name":name,"size":7,"sha256":model::hash("1234567")});
    let (a, b) = tokio::join!(
        h.json("POST", "/api/live/uploads", &cookie, input("a")),
        h.json("POST", "/api/live/uploads", &cookie, input("b"))
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::PAYLOAD_TOO_LARGE)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::PAYLOAD_TOO_LARGE)
    );
    let winner = if a.0 == StatusCode::OK { a.1 } else { b.1 };
    let id = winner["id"].as_str().unwrap();
    let (_, me) = h.json("GET", "/api/live/me", &cookie, Value::Null).await;
    assert_eq!(me["reserved_bytes"], 7);
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/uploads/{id}"),
            &cookie,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(!storage::upload_path(&h.app, id).exists());
    let id = h.file(&cookie, "ten", b"0123456789").await;
    assert_eq!(
        h.json("POST", "/api/live/uploads", &cookie, input("more"))
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    h.json(
        "DELETE",
        &format!("/api/live/files/{id}"),
        &cookie,
        Value::Null,
    )
    .await;
    assert_eq!(
        h.json("POST", "/api/live/uploads", &cookie, input("more"))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn checksum_offset_and_size_are_enforced() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.start(&cookie, "a", b"correct").await;
    assert_eq!(
        h.chunk(&cookie, &id, 1, b"wrong").await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.chunk(&cookie, &id, 0, b"too many bytes").await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.request(
            "PUT",
            &format!("/api/live/uploads/{id}"),
            &cookie,
            b"wrong".to_vec(),
            &[("upload-offset", "0"), ("upload-checksum", "bad")]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::CONFLICT);
    assert_eq!(h.chunk(&cookie, &id, 0, b"corrupt").await, StatusCode::OK);
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::OK);
    h.drain().await;
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files[0]["status"], "failed");
    assert!(
        files[0]["processing_error"]
            .as_str()
            .unwrap()
            .contains("checksum")
    );
    let (status, _) = h
        .json(
            "POST",
            "/api/live/uploads",
            &cookie,
            json!({"name":"a","size":7,"sha256":model::hash("another")}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        h.request(
            "PUT",
            &format!("/api/live/uploads/{id}"),
            &cookie,
            vec![0; amalthea::transfers::CHUNK_BYTES + 1],
            &[]
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn upload_survives_reopen_and_uncommitted_disk_tail() {
    let mut h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.start(&cookie, "a.txt", b"hello world").await;
    assert_eq!(h.chunk(&cookie, &id, 0, b"hello").await, StatusCode::OK);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(storage::upload_path(&h.app, &id))
        .unwrap()
        .write_all(b"uncommitted garbage")
        .unwrap();
    h.app.db.close().await;
    h.app = App::open(h.app.settings.clone()).await.unwrap();
    h.router = routes::router(h.app.clone());
    let (_, state) = h
        .json(
            "GET",
            &format!("/api/live/uploads/{id}"),
            &cookie,
            Value::Null,
        )
        .await;
    assert_eq!(state["offset"], 5);
    assert_eq!(
        h.chunk(&cookie, &id, 0, b"hello").await,
        StatusCode::CONFLICT
    );
    assert_eq!(h.chunk(&cookie, &id, 5, b" world").await, StatusCode::OK);
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::OK);
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::OK);
    h.drain().await;
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/live/files/{id}/content"),
            &cookie,
            vec![],
            &[]
        )
        .await
        .2,
        b"hello world"
    );
    storage::maintenance(&h.app).await.unwrap();
    assert_eq!(
        h.json(
            "GET",
            &format!("/api/live/files/{id}"),
            &cookie,
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn independent_links_expire_revoke_and_log_without_exposing_owner() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let other = h.seed("Bob", 100).await;
    let id = h.file(&cookie, "a.txt", b"hello world").await;
    let (link, token) = h.link(&cookie, &id).await;
    let (second, token2) = h.link(&cookie, &id).await;
    assert_eq!(token.len(), 86);
    let (status, meta) = h
        .json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(meta.get("user_id").is_none() && meta.get("username").is_none());
    let (status, headers, body) = h
        .request(
            "GET",
            &format!("/api/live/public/{token}/content"),
            "",
            vec![],
            &[("range", "bytes=1-4")],
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers["content-range"], "bytes 1-4/11");
    assert_eq!(body, b"ello");
    let (_, logs) = h
        .json(
            "GET",
            &format!("/api/live/links/{link}/logs"),
            &cookie,
            Value::Null,
        )
        .await;
    assert_eq!(logs.as_array().unwrap().len(), 2);
    assert_eq!(logs[0]["event"], "download");
    assert_eq!(logs[1]["event"], "view");
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/links/{link}"),
            &other,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json(
            "GET",
            &format!("/api/live/links/{link}/logs"),
            &other,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/links/{link}"),
            &cookie,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        h.json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json(
            "GET",
            &format!("/api/live/public/{token2}"),
            "",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE links SET expires_at = ? WHERE id = ?")
        .bind(model::now() - 1)
        .bind(second)
        .execute(&h.app.db)
        .await
        .unwrap();
    assert_eq!(
        h.json(
            "GET",
            &format!("/api/live/public/{token2}"),
            "",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ranges_etags_head_and_empty_files() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.file(&cookie, "a.txt", b"0123456789").await;
    let path = format!("/api/live/files/{id}/content");
    for (range, expected) in [
        ("bytes=2-4", b"234".as_slice()),
        ("bytes=7-", b"789"),
        ("bytes=-3", b"789"),
        ("bytes=8-100", b"89"),
    ] {
        let (status, _, body) = h
            .request("GET", &path, &cookie, vec![], &[("range", range)])
            .await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(body, expected);
    }
    for range in [
        "bytes=10-",
        "bytes=5-2",
        "bytes=0-1,3-4",
        "bytes=-0",
        "junk",
    ] {
        let (status, headers, _) = h
            .request("GET", &path, &cookie, vec![], &[("range", range)])
            .await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers["content-range"], "bytes */10");
    }
    assert_eq!(
        h.request("GET", &path, &cookie, vec![], &[("if-match", "\"old\"")])
            .await
            .0,
        StatusCode::PRECONDITION_FAILED
    );
    let (status, _, body) = h
        .request(
            "GET",
            &path,
            &cookie,
            vec![],
            &[("if-range", "\"old\""), ("range", "bytes=2-3")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"0123456789");
    let (_, headers, body) = h.request("HEAD", &path, &cookie, vec![], &[]).await;
    assert_eq!(headers["content-length"], "10");
    assert!(body.is_empty());
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    let empty = h.file(&cookie, "empty", b"").await;
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/live/files/{empty}/content"),
            &cookie,
            vec![],
            &[]
        )
        .await
        .2,
        b""
    );
    assert_eq!(
        amalthea::transfers::byte_range("bytes=17179869180-", 17179869184),
        Some((17179869180, 17179869183))
    );
}

#[tokio::test]
async fn rename_preserves_links_delete_removes_everything() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.file(&cookie, "a.txt", b"hello").await;
    let (_, token) = h.link(&cookie, &id).await;
    assert_eq!(
        h.json(
            "PATCH",
            &format!("/api/live/files/{id}"),
            &cookie,
            json!({"name":"renamed.txt"})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    storage::maintenance(&h.app).await.unwrap();
    let (status, meta) = h
        .json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["name"], "renamed.txt");
    let _ = h.file(&cookie, "taken", b"other").await;
    assert_eq!(
        h.json(
            "PATCH",
            &format!("/api/live/files/{id}"),
            &cookie,
            json!({"name":"taken"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    h.start(&cookie, "reserved", b"pending").await;
    assert_eq!(
        h.json(
            "PATCH",
            &format!("/api/live/files/{id}"),
            &cookie,
            json!({"name":"reserved"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/files/{id}"),
            &cookie,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(
        !h.app
            .settings
            .data_dir
            .join("users/Alice/renamed.txt")
            .exists()
    );
    assert_eq!(
        h.json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM links")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn reconciliation_imports_removes_and_replaces_without_reusing_links() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let dir = h.app.settings.data_dir.join("users/Alice");
    std::fs::write(dir.join("manual.txt"), b"first").unwrap();
    std::fs::create_dir(dir.join("nested")).unwrap();
    std::os::unix::fs::symlink(dir.join("manual.txt"), dir.join("symlink")).unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files.as_array().unwrap().len(), 1);
    let id = files[0]["id"].as_str().unwrap();
    let (_, token) = h.link(&cookie, id).await;
    std::fs::write(dir.join("manual.txt"), b"replacement").unwrap();
    assert_eq!(
        h.json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_ne!(files[0]["id"], id);
    assert_eq!(files[0]["size"], 11);
    assert_eq!(
        h.json("GET", &format!("/api/live/public/{token}"), "", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    std::fs::remove_file(dir.join("manual.txt")).unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files, json!([]));
}

#[tokio::test]
async fn incomplete_imports_require_explicit_repair_and_remain_deletable() {
    use std::os::unix::ffi::OsStringExt;
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 1000).await;
    let other = h.seed("Bob", 1000).await;
    let dir = h.app.settings.data_dir.join("users/Alice");
    let names = vec![
        "bad name.txt".into(),
        "with-dash".into(),
        ".hidden".into(),
        "café.txt".into(),
        "quote\"newline\n".into(),
        "a".repeat(201).into(),
        std::ffi::OsString::from_vec(b"raw\xff".to_vec()),
        std::ffi::OsString::from_vec(b"raw\xfe".to_vec()),
    ];
    for (i, name) in names.iter().enumerate() {
        std::fs::write(dir.join(name), [i as u8]).unwrap();
    }
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files.as_array().unwrap().len(), names.len());
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["incomplete"] == true)
    );
    let (_, again) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files, again);
    let (_, me) = h.json("GET", "/api/live/me", &cookie, Value::Null).await;
    assert_eq!(me["used_bytes"], names.len());
    for file in files.as_array().unwrap() {
        let id = file["id"].as_str().unwrap();
        let path = format!("/api/live/files/{id}");
        assert_eq!(
            h.request("GET", &format!("{path}/content"), &cookie, vec![], &[])
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            h.json("POST", &format!("{path}/links"), &cookie, json!({}))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            h.json("PATCH", &path, &other, json!({"name":"fixed"}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.json("DELETE", &path, &other, Value::Null).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            h.json("PATCH", &path, &cookie, json!({"name":"still bad"}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for name in &names {
        assert!(dir.join(name).exists());
    }
    h.drain().await;
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    // Use hashes to distinguish even raw filenames with the same lossy display name.
    for (i, name) in names.iter().enumerate() {
        let file = files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["sha256"] == model::hash([i as u8]))
            .unwrap();
        let id = file["id"].as_str().unwrap();
        let path = format!("/api/live/files/{id}");
        if i % 2 == 0 {
            let fixed = format!("fixed_{i}.txt");
            assert_eq!(
                h.json("PATCH", &path, &cookie, json!({"name":fixed}))
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
            let (_, updated) = h.json("GET", &path, &cookie, Value::Null).await;
            assert_eq!(updated["incomplete"], false);
            assert_eq!(updated["id"], id);
            assert_eq!(updated["name"], fixed);
            assert_eq!(
                h.request("GET", &format!("{path}/content"), &cookie, vec![], &[])
                    .await
                    .2,
                vec![i as u8]
            );
            h.link(&cookie, id).await;
        } else {
            assert_eq!(
                h.json("DELETE", &path, &cookie, Value::Null).await.0,
                StatusCode::NO_CONTENT
            );
        }
        assert!(!dir.join(name).exists());
    }
    let (_, remaining) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(remaining.as_array().unwrap().len(), names.len() / 2);
    assert!(
        remaining
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["incomplete"] == false)
    );
}

#[tokio::test]
async fn incomplete_repairs_do_not_overwrite_files_or_reserved_uploads() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    h.start(&cookie, "reserved.txt", b"pending").await;
    let dir = h.app.settings.data_dir.join("users/Alice");
    std::fs::write(dir.join("bad name"), b"original").unwrap();
    std::fs::write(dir.join("existing.txt"), b"existing").unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    let file = files
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["incomplete"] == true)
        .unwrap();
    let path = format!("/api/live/files/{}", file["id"].as_str().unwrap());
    for name in ["existing.txt", "reserved.txt"] {
        assert_eq!(
            h.json("PATCH", &path, &cookie, json!({"name":name}))
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(std::fs::read(dir.join("bad name")).unwrap(), b"original");
    assert_eq!(
        std::fs::read(dir.join("existing.txt")).unwrap(),
        b"existing"
    );
}

#[tokio::test]
async fn publication_recovery_and_expired_upload_cleanup() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.start(&cookie, "a", b"hello").await;
    h.chunk(&cookie, &id, 0, b"hello").await;
    std::fs::hard_link(
        storage::upload_path(&h.app, &id),
        h.app.settings.data_dir.join("users/Alice/a"),
    )
    .unwrap();
    storage::maintenance(&h.app).await.unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files[0]["id"], id);
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::OK);
    assert_eq!(files[0]["status"], "processing");
    assert!(storage::upload_path(&h.app, &id).exists());
    // Maintenance must leave queued recovery cleanup to the worker pool.
    storage::maintenance(&h.app).await.unwrap();
    assert!(storage::upload_path(&h.app, &id).exists());
    h.drain().await;
    assert!(!storage::upload_path(&h.app, &id).exists());
    let stale = h.start(&cookie, "stale", b"pending").await;
    sqlx::query("UPDATE uploads SET updated_at = 0")
        .execute(&h.app.db)
        .await
        .unwrap();
    std::fs::write(h.app.settings.data_dir.join("uploads/orphan"), b"orphan").unwrap();
    storage::maintenance(&h.app).await.unwrap();
    assert!(!storage::upload_path(&h.app, &stale).exists());
    assert!(!h.app.settings.data_dir.join("uploads/orphan").exists());
    let (_, me) = h.json("GET", "/api/live/me", &cookie, Value::Null).await;
    assert_eq!(me["reserved_bytes"], 0);
}

#[tokio::test]
async fn csrf_canonical_urls_and_subpath_assets() {
    let h = Harness::with_prefix("/share").await;
    let cookie = h.seed("Alice", 100).await;
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/share/api/live/logout")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        h.request(
            "POST",
            "/api/live/logout",
            &cookie,
            vec![],
            &[("origin", "https://evil.test")]
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for (path, to) in [
        ("/Alice", "/share/Alice/"),
        ("/alice/", "/share/Alice/"),
        ("/register/", "/share/register"),
    ] {
        let (status, headers, _) = h.request("GET", path, &cookie, vec![], &[]).await;
        assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(headers["location"], to);
    }
    let (status, _, body) = h.request("GET", "/Alice/", &cookie, vec![], &[]).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("/share/res/static/src/main.js"));
    assert!(!body.contains("__AMALTHEA_BASE__"));
    assert_eq!(
        h.request("GET", "/res/static/src/main.js", "", vec![], &[])
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        h.request("GET", "/api/live/unknown", "", vec![], &[])
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cli_quota_changes_preserve_existing_data_and_block_completion() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.file(&cookie, "existing", b"hello").await;
    let pending = h.start(&cookie, "pending", b"more").await;
    h.chunk(&cookie, &pending, 0, b"more").await;
    let config = h._dir.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(
            &json!({"data_dir":h.app.settings.data_dir,"static_dir":h.app.settings.static_dir}),
        )
        .unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_amalthea"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "quota",
            "alice",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        h.finish(&cookie, &pending).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        h.request(
            "GET",
            &format!("/api/live/files/{id}/content"),
            &cookie,
            vec![],
            &[]
        )
        .await
        .2,
        b"hello"
    );
    let (_, me) = h.json("GET", "/api/live/me", &cookie, Value::Null).await;
    assert_eq!(me["user"]["quota_bytes"], 1);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_amalthea"))
        .args([
            "--config",
            config.to_str().unwrap(),
            "user",
            "create",
            "--quota",
            "200",
            "--expires-hours",
            "1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("/register#"));
}

#[tokio::test]
async fn rejects_public_data_directory_and_symlink_user_directory() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let mut bad = h.app.settings.clone();
    bad.static_dir = h._dir.path().to_owned();
    assert!(App::open(bad).await.is_err());
    let path = h.app.settings.data_dir.join("users/Alice");
    std::fs::remove_dir(&path).unwrap();
    std::os::unix::fs::symlink(h._dir.path(), path).unwrap();
    assert_eq!(
        h.json("GET", "/api/live/files", &cookie, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn cli_accepts_readable_quotas_and_prints_both_units() {
    let h = Harness::new().await;
    h.seed("Alice", 100).await;
    let config = h._dir.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(
            &json!({"data_dir":h.app.settings.data_dir,"static_dir":h.app.settings.static_dir}),
        )
        .unwrap(),
    )
    .unwrap();
    let cli = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_amalthea"))
            .args(["--config", config.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    for (value, formatted) in [
        ("1.5G", "1610612736 (1.5 GiB)"),
        ("20M", "20971520 (20 MiB)"),
        ("3K", "3072 (3 KiB)"),
        ("53687091200", "53687091200 (50 GiB)"),
    ] {
        assert_eq!(
            cli(&["user", "quota", "alice", value]),
            format!("alice\t{formatted}\n")
        );
        assert_eq!(
            cli(&["user", "quota", "alice"]),
            format!("alice\t{formatted}\n")
        );
    }
    assert!(cli(&["user", "list"]).contains("Alice\t53687091200 (50 GiB)\t0 (0 B)\t0 (0 B)"));
    cli(&["user", "create", "--quota", "1.5G"]);
    let quota: i64 = sqlx::query_scalar("SELECT quota_bytes FROM invitations")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(quota, 1_610_612_736);
}

#[tokio::test]
async fn imprint_fragment_and_private_fallback() {
    let h = Harness::with_prefix("/share").await;
    let (_, _, body) = h.request("GET", "/login", "", vec![], &[]).await;
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("name=\"amalthea-imprint\" content=\"false\"")
    );
    assert_eq!(
        h.request("GET", "/imprint", "", vec![], &[]).await.0,
        StatusCode::NOT_FOUND
    );
    std::fs::write(
        h.app.settings.imprint.as_ref().unwrap(),
        "<h1>Example operator</h1>",
    )
    .unwrap();
    let (status, _, body) = h.request("GET", "/imprint", "", vec![], &[]).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("<h1>Example operator</h1>"));
    assert!(body.contains("name=\"amalthea-imprint\" content=\"true\""));
    assert!(body.contains("/share/res/static/src/main.js"));
    let (status, headers, _) = h.request("GET", "/imprint/", "", vec![], &[]).await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(headers["location"], "/share/imprint");
    std::fs::write(h.app.settings.imprint.as_ref().unwrap(), "  ").unwrap();
    assert_eq!(
        h.request("GET", "/imprint", "", vec![], &[]).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn imprint_disabled_shows_private_even_when_example_exists() {
    let mut h = Harness::new().await;
    let mut settings = h.app.settings.clone();
    settings.imprint = None;
    h.app = App::open(settings).await.unwrap();
    h.router = routes::router(h.app.clone());
    let (status, _, body) = h.request("GET", "/login", "", vec![], &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("name=\"amalthea-imprint\" content=\"false\"")
    );
    assert_eq!(
        h.request("GET", "/imprint", "", vec![], &[]).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn processing_queue_is_durable_fair_and_restricts_operations() {
    let mut h = Harness::new().await;
    let a = h.seed("Alice", 1000).await;
    let b = h.seed("Bob", 1000).await;
    for name in ["one", "two", "three"] {
        std::fs::write(
            h.app.settings.data_dir.join("users/Alice").join(name),
            b"abc",
        )
        .unwrap();
    }
    std::fs::write(h.app.settings.data_dir.join("users/Bob/one"), b"abc").unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &a, Value::Null).await;
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "processing" && f["sha256"] == "")
    );
    h.json("GET", "/api/live/files", &b, Value::Null).await;
    let id = files[0]["id"].as_str().unwrap();
    let path = format!("/api/live/files/{id}");
    for (method, suffix, body) in [
        ("GET", "/content", Value::Null),
        ("GET", "/links", Value::Null),
        ("POST", "/links", json!({})),
        ("PATCH", "", json!({"name":"renamed"})),
    ] {
        assert_eq!(
            h.json(method, &format!("{path}{suffix}"), &a, body).await.0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        h.json("DELETE", &path, &b, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.json("DELETE", &path, &a, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );

    h.app.db.close().await;
    h.app = App::open(h.app.settings.clone()).await.unwrap();
    h.router = routes::router(h.app.clone());
    let mut active = std::collections::HashSet::new();
    let first = amalthea::processing::next(&h.app, 0, &active)
        .await
        .unwrap()
        .unwrap();
    active.insert(first.id.clone());
    let second = amalthea::processing::next(&h.app, first.user_id, &active)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.user_id, second.user_id);
    active.insert(second.id.clone());
    let third = amalthea::processing::next(&h.app, second.user_id, &active)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.user_id, first.user_id);
    h.drain().await;
    let (_, files) = h.json("GET", "/api/live/files", &a, Value::Null).await;
    assert_eq!(files.as_array().unwrap().len(), 2);
    assert!(
        files
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["status"] == "ready" && f["sha256"] == model::hash(b"abc"))
    );
}

#[tokio::test]
async fn processing_failure_retry_and_deleted_job_cannot_resurrect_file() {
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    std::fs::write(h.app.settings.data_dir.join("users/Alice/a"), b"abc").unwrap();
    h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    let file = amalthea::processing::next(&h.app, 0, &Default::default())
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE files SET status = 'failed', processing_error = 'Temporary failure' WHERE id = ?",
    )
    .bind(&file.id)
    .execute(&h.app.db)
    .await
    .unwrap();
    let path = format!("/api/live/files/{}", file.id);
    assert_eq!(
        h.json("POST", &format!("{path}/retry"), &cookie, Value::Null)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    h.drain().await;
    let (_, ready) = h.json("GET", &path, &cookie, Value::Null).await;
    assert_eq!(ready["status"], "ready");
    assert!(ready["processing_error"].is_null());
    assert_eq!(
        h.json("POST", &format!("{path}/retry"), &cookie, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.json("DELETE", &path, &cookie, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    // Simulate a previously dispatched worker completing after deletion.
    amalthea::processing::process(&h.app, &file, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        h.json("GET", &path, &cookie, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn worker_limit_and_cancellation_keep_service_responsive() {
    use std::time::Duration;
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 1024 * 1024 * 1024).await;
    for name in ["a", "b", "c", "d"] {
        std::fs::File::create(h.app.settings.data_dir.join("users/Alice").join(name))
            .unwrap()
            .set_len(128 * 1024 * 1024)
            .unwrap();
    }
    let (_, files) = tokio::time::timeout(
        Duration::from_secs(2),
        h.json("GET", "/api/live/files", &cookie, Value::Null),
    )
    .await
    .unwrap();
    assert_eq!(files.as_array().unwrap().len(), 4);
    let dispatcher = amalthea::processing::start(h.app.clone());
    let running = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let running = h.app.processing.lock().unwrap().clone();
            assert!(running.len() <= h.app.settings.processing_workers);
            if running.len() == h.app.settings.processing_workers {
                break running;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // This request takes the same mutation lock that hashing used to monopolize.
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            h.json("GET", "/api/live/me", &cookie, Value::Null)
        )
        .await
        .unwrap()
        .0,
        StatusCode::OK
    );
    let (id, token) = running.iter().next().unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            h.json(
                "DELETE",
                &format!("/api/live/files/{id}"),
                &cookie,
                Value::Null
            )
        )
        .await
        .unwrap()
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(token.is_cancelled());
    dispatcher.abort();
    let _ = dispatcher.await;
    for token in running.values() {
        token.cancel();
    }
}

#[tokio::test]
async fn separate_uploads_write_without_the_global_lock_and_status_is_lock_free() {
    use std::time::Duration;
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let a = h.start(&cookie, "a", b"abc").await;
    let b = h.start(&cookie, "b", b"xyz").await;
    let upload_guard = h.app.lock_upload(&a).await;
    let metadata_guard = h.app.mutation.lock().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), h.chunk(&cookie, &b, 0, b"xyz"))
            .await
            .unwrap(),
        StatusCode::OK
    );
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(2),
        h.json(
            "GET",
            &format!("/api/live/uploads/{a}"),
            &cookie,
            Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["offset"], 0);
    drop(metadata_guard);
    // Cancellation waiting for A must not monopolize metadata access.
    let path = format!("/api/live/uploads/{a}");
    let cancel = h.json("DELETE", &path, &cookie, Value::Null);
    tokio::pin!(cancel);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut cancel)
            .await
            .is_err()
    );
    drop(
        tokio::time::timeout(Duration::from_secs(2), h.app.mutation.lock())
            .await
            .unwrap(),
    );
    drop(upload_guard);
    assert_eq!(cancel.await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn chunks_serialize_per_upload_and_survive_disconnected_requests() {
    use std::time::Duration;
    let h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.start(&cookie, "a", b"abc").await;
    let (first, second) = tokio::join!(
        h.chunk(&cookie, &id, 0, b"abc"),
        h.chunk(&cookie, &id, 0, b"abc")
    );
    assert!(
        (first == StatusCode::OK && second == StatusCode::CONFLICT)
            || (second == StatusCode::OK && first == StatusCode::CONFLICT)
    );
    assert_eq!(
        std::fs::read(storage::upload_path(&h.app, &id)).unwrap(),
        b"abc"
    );

    let id = h.start(&cookie, "b", b"xyz").await;
    let guard = h.app.lock_upload(&id).await;
    // Dropping the request after it queues its write simulates disconnect.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), h.chunk(&cookie, &id, 0, b"xyz"))
            .await
            .is_err()
    );
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (_, state) = h
                .json(
                    "GET",
                    &format!("/api/live/uploads/{id}"),
                    &cookie,
                    Value::Null,
                )
                .await;
            if state["offset"] == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(storage::upload_path(&h.app, &id)).unwrap(),
        b"xyz"
    );
}

#[tokio::test]
async fn recovery_job_resumes_after_cleanup_crash_and_can_be_deleted() {
    let mut h = Harness::new().await;
    let cookie = h.seed("Alice", 100).await;
    let id = h.start(&cookie, "a", b"abc").await;
    h.chunk(&cookie, &id, 0, b"abc").await;
    std::fs::hard_link(
        storage::upload_path(&h.app, &id),
        h.app.settings.data_dir.join("users/Alice/a"),
    )
    .unwrap();
    storage::maintenance(&h.app).await.unwrap();
    // Crash after unlinking the temporary name, before marking the job ready.
    std::fs::remove_file(storage::upload_path(&h.app, &id)).unwrap();
    h.app.db.close().await;
    h.app = App::open(h.app.settings.clone()).await.unwrap();
    h.router = routes::router(h.app.clone());
    storage::maintenance(&h.app).await.unwrap();
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files[0]["id"], id);
    assert_eq!(files[0]["status"], "processing");
    h.drain().await;
    let (_, ready) = h
        .json(
            "GET",
            &format!("/api/live/files/{id}"),
            &cookie,
            Value::Null,
        )
        .await;
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["sha256"], model::hash(b"abc"));

    let id = h.start(&cookie, "b", b"abc").await;
    h.chunk(&cookie, &id, 0, b"abc").await;
    let destination = h.app.settings.data_dir.join("users/Alice/b");
    std::fs::hard_link(storage::upload_path(&h.app, &id), &destination).unwrap();
    storage::maintenance(&h.app).await.unwrap();
    assert!(storage::upload_path(&h.app, &id).exists());
    assert_eq!(
        h.json(
            "DELETE",
            &format!("/api/live/files/{id}"),
            &cookie,
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(!destination.exists());
    assert!(!storage::upload_path(&h.app, &id).exists());
}
