use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Generates a new API key: 32 random bytes, hex-encoded. The plaintext key
/// exists only in the return value; the database stores only its hash.
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}

/// The SHA-256 hash of an API key, hex-encoded: the form stored in
/// `members.api_key_hash` and looked up when a key is presented.
pub fn hash(api_key: &str) -> String {
    hex::encode(Sha256::digest(api_key.as_bytes()))
}

/// Clears the member's stored key hash so their key no longer authenticates.
/// Returns false when no member has that id.
pub async fn revoke(pool: &SqlitePool, member_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("UPDATE members SET api_key_hash = NULL WHERE id = ?")
        .bind(member_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}
