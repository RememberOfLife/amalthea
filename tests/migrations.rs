use sqlx::sqlite::SqlitePoolOptions;

#[tokio::test]
async fn username_migration_preserves_invitations_and_updates_validation() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_initial.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO invitations VALUES ('assigned', 'assigned-token', 'Alice', 100, 123), ('open', 'open-token', NULL, 200, 456)")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_username_punctuation.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let invitations: Vec<(String, String, Option<String>, i64, i64)> =
        sqlx::query_as("SELECT token_hash, token, username, quota_bytes, expires_at FROM invitations ORDER BY token_hash")
            .fetch_all(&pool).await.unwrap();
    assert_eq!(
        invitations,
        vec![
            (
                "assigned".into(),
                "assigned-token".into(),
                Some("Alice".into()),
                100,
                123
            ),
            ("open".into(), "open-token".into(), None, 200, 456),
        ]
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM invitations WHERE username = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    for (i, name) in [
        "Alice.Smith-family+tag@example.org",
        "a..b",
        "a.",
        "-",
        "@",
        "+",
        &"a".repeat(64),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query("INSERT INTO invitations VALUES (?, ?, ?, 0, 999)")
            .bind(format!("hash-{i}"))
            .bind(format!("token-{i}"))
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    for name in [
        "",
        ".",
        "..",
        ".hidden",
        "...",
        "../escape",
        "a/b",
        "a\\b",
        "a b",
        "a\n",
        "a\0",
        "a#b",
        "ä",
        &"a".repeat(65),
    ] {
        assert!(
            sqlx::query("INSERT INTO invitations VALUES ('invalid', 'invalid-token', ?, 0, 999)")
                .bind(name)
                .execute(&pool)
                .await
                .is_err(),
            "{name:?}"
        );
    }
}
