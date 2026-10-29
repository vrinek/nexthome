use sqlx::{Executor, Sqlite};

/// Which provider an API request went to: the `api_usage.provider` value.
#[derive(Clone, Copy, Debug)]
pub enum Provider {
    Mapbox,
    Google,
}

impl Provider {
    /// The value stored in `api_usage.provider`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mapbox => "mapbox",
            Self::Google => "google",
        }
    }
}

/// Counts one provider request against the relocation for the current
/// month: the first request inserts the row, later ones bump `calls`.
pub async fn record_request<'e, E>(
    executor: E,
    relocation_id: i64,
    provider: Provider,
) -> Result<(), sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    sqlx::query(
        "INSERT INTO api_usage (relocation_id, provider, month, calls)
         VALUES (?, ?, strftime('%Y%m', 'now'), 1)
         ON CONFLICT (relocation_id, provider, month)
         DO UPDATE SET calls = calls + 1",
    )
    .bind(relocation_id)
    .bind(provider.as_str())
    .execute(executor)
    .await?;
    Ok(())
}
