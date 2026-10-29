use axum::{
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
};
use sqlx::SqlitePool;

use crate::api_keys;

/// The member and account that a valid `Authorization: Bearer` API key
/// resolves to. Extracting it answers 401 for a missing, malformed, unknown
/// or revoked key.
pub struct AuthenticatedMember {
    pub account_id: i64,
    pub member_id: i64,
    pub email: String,
}

impl<S> FromRequestParts<S> for AuthenticatedMember
where
    SqlitePool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = SqlitePool::from_ref(state);
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let (scheme, key) = header.split_once(' ').ok_or(StatusCode::UNAUTHORIZED)?;
        if !scheme.eq_ignore_ascii_case("bearer") || key.is_empty() {
            return Err(StatusCode::UNAUTHORIZED);
        }

        let member = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT account_id, id, email FROM members WHERE api_key_hash = ?",
        )
        .bind(api_keys::hash(key))
        .fetch_optional(&pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "api key lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        member
            .map(|(account_id, member_id, email)| AuthenticatedMember {
                account_id,
                member_id,
                email,
            })
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}
