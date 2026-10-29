use axum::{
    extract::{FromRef, FromRequestParts},
    http::{
        HeaderValue, StatusCode,
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
        request::Parts,
    },
    response::{IntoResponse, Response},
};
use sqlx::SqlitePool;

use crate::api_keys;

/// The member and account that a valid `Authorization: Bearer` API key
/// resolves to.
pub struct AuthenticatedMember {
    pub account_id: i64,
    pub member_id: i64,
    pub email: String,
}

/// Why a request did not authenticate: no usable key, or a key lookup that
/// itself failed. Renders as 401 with `WWW-Authenticate: Bearer` (RFC 7235)
/// and as 500 respectively.
pub enum AuthRejection {
    /// The key is missing, malformed, unknown or revoked.
    NoUsableKey,
    /// Looking up the key's hash in the database failed.
    LookupFailed,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match self {
            Self::NoUsableKey => {
                let mut response = StatusCode::UNAUTHORIZED.into_response();
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                response
            }
            Self::LookupFailed => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

impl<S> FromRequestParts<S> for AuthenticatedMember
where
    SqlitePool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = SqlitePool::from_ref(state);
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthRejection::NoUsableKey)?;
        let (scheme, key) = header.split_once(' ').ok_or(AuthRejection::NoUsableKey)?;
        if !scheme.eq_ignore_ascii_case("bearer") || key.is_empty() {
            return Err(AuthRejection::NoUsableKey);
        }

        let member = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT account_id, id, email FROM members WHERE api_key_hash = ?",
        )
        .bind(api_keys::hash(key))
        .fetch_optional(&pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "api key lookup failed");
            AuthRejection::LookupFailed
        })?;

        member
            .map(|(account_id, member_id, email)| AuthenticatedMember {
                account_id,
                member_id,
                email,
            })
            .ok_or(AuthRejection::NoUsableKey)
    }
}
