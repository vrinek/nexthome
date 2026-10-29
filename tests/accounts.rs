use std::str::FromStr;

use nexthome::{accounts, api_keys};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

async fn migrated_pool() -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    // SQLite enforces foreign keys only when the connection opts in, so the
    // duplicate-email and membership assertions below depend on this option.
    let options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    (directory, pool)
}

#[tokio::test]
async fn creating_an_account_adds_its_first_member_and_returns_the_key_once() {
    let (_directory, pool) = migrated_pool().await;

    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();

    assert_eq!(created.email, "mover@example.com");
    assert_eq!(created.api_key.len(), 64);
    assert!(created.api_key.chars().all(|c| c.is_ascii_hexdigit()));

    let (account_id, api_key_hash, created_at): (i64, String, String) =
        sqlx::query_as("SELECT account_id, api_key_hash, created_at FROM members WHERE id = ?")
            .bind(created.member_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(account_id, created.account_id);
    // The plaintext key appears only in the return value; the database keeps
    // only its hash.
    assert_eq!(api_key_hash, api_keys::hash(&created.api_key));
    // created_at comes from SQLite's strftime; the shape check catches a
    // format-string typo.
    assert_eq!(created_at.len(), 20);
    assert!(created_at.ends_with('Z'));

    let account_created_at: String =
        sqlx::query_scalar("SELECT created_at FROM accounts WHERE id = ?")
            .bind(created.account_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(account_created_at.len(), 20);
    assert!(account_created_at.ends_with('Z'));

    pool.close().await;
}

#[tokio::test]
async fn add_member_gives_an_account_a_second_member_with_their_own_key() {
    let (_directory, pool) = migrated_pool().await;

    let first = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let second = accounts::add_member(&pool, first.account_id, "partner@example.com")
        .await
        .unwrap();

    assert_eq!(second.email, "partner@example.com");
    assert_ne!(second.member_id, first.member_id);
    assert_ne!(second.api_key, first.api_key);
    assert_ne!(
        api_keys::hash(&second.api_key),
        api_keys::hash(&first.api_key)
    );

    let members: i64 = sqlx::query_scalar("SELECT count(*) FROM members WHERE account_id = ?")
        .bind(first.account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(members, 2);

    let stored_hash: String = sqlx::query_scalar("SELECT api_key_hash FROM members WHERE id = ?")
        .bind(second.member_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_hash, api_keys::hash(&second.api_key));

    pool.close().await;
}

#[tokio::test]
async fn revoking_a_key_clears_its_hash() {
    let (_directory, pool) = migrated_pool().await;

    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    api_keys::revoke(&pool, created.member_id).await.unwrap();

    let api_key_hash: Option<String> =
        sqlx::query_scalar("SELECT api_key_hash FROM members WHERE id = ?")
            .bind(created.member_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(api_key_hash, None);

    pool.close().await;
}

#[tokio::test]
async fn duplicate_emails_are_rejected() {
    let (_directory, pool) = migrated_pool().await;

    let first = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let error = accounts::add_member(&pool, first.account_id, "mover@example.com")
        .await
        .err()
        .expect("a second member with an existing email must be rejected");
    assert!(
        error
            .to_string()
            .contains("UNIQUE constraint failed: members.email"),
        "a second member with an existing email must violate the unique constraint: {error}"
    );

    pool.close().await;
}
