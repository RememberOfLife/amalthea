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
    async fn file(&self, cookie: &str, name: &str, bytes: &[u8]) -> String {
        let id = self.start(cookie, name, bytes).await;
        if !bytes.is_empty() {
            assert_eq!(self.chunk(cookie, &id, 0, bytes).await, StatusCode::OK);
        }
        let (status, body) = self.finish(cookie, &id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        id
    }
    async fn link(&self, cookie: &str, id: &str) -> (String, String) {
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
    assert_eq!(h.finish(&cookie, &id).await.0, StatusCode::CONFLICT);
    let (_, files) = h.json("GET", "/api/live/files", &cookie, Value::Null).await;
    assert_eq!(files, json!([]));
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
