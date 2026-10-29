use sha2::{Digest, Sha256};

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
