use sqlx::{Executor, Sqlite, SqlitePool};

use crate::api_keys;

/// A new account with its first member. `api_key` is that member's key in
/// plaintext — the only time it is ever available; the database keeps only
/// its hash.
pub struct CreatedAccount {
    pub account_id: i64,
    pub member_id: i64,
    pub email: String,
    pub api_key: String,
}

/// A new member on an existing account. As with account creation, `api_key`
/// is the plaintext key, available only here.
pub struct CreatedMember {
    pub member_id: i64,
    pub email: String,
    pub api_key: String,
}

/// Creates an account and its first member, who receives a fresh API key.
pub async fn create_with_first_member(
    pool: &SqlitePool,
    email: &str,
) -> Result<CreatedAccount, sqlx::Error> {
    let api_key = api_keys::generate();
    let mut transaction = pool.begin().await?;

    let account_id: i64 = sqlx::query_scalar(
        "INSERT INTO accounts (created_at)
         VALUES (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) RETURNING id",
    )
    .fetch_one(&mut *transaction)
    .await?;
    let member_id = insert_member(&mut *transaction, account_id, email, &api_key).await?;

    transaction.commit().await?;
    Ok(CreatedAccount {
        account_id,
        member_id,
        email: email.to_owned(),
        api_key,
    })
}

/// Adds a member with their own API key to an existing account.
pub async fn add_member(
    pool: &SqlitePool,
    account_id: i64,
    email: &str,
) -> Result<CreatedMember, sqlx::Error> {
    let api_key = api_keys::generate();
    let mut transaction = pool.begin().await?;

    let member_id = insert_member(&mut *transaction, account_id, email, &api_key).await?;

    transaction.commit().await?;
    Ok(CreatedMember {
        member_id,
        email: email.to_owned(),
        api_key,
    })
}

async fn insert_member<'e, E>(
    executor: E,
    account_id: i64,
    email: &str,
    api_key: &str,
) -> Result<i64, sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    let member_id: i64 = sqlx::query_scalar(
        "INSERT INTO members (account_id, email, api_key_hash, created_at)
         VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')) RETURNING id",
    )
    .bind(account_id)
    .bind(email)
    .bind(api_keys::hash(api_key))
    .fetch_one(executor)
    .await?;
    Ok(member_id)
}
