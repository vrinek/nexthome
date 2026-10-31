use std::{env, fmt};

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

impl fmt::Display for Provider {
    // The name for messages a person reads; the stored value comes from
    // `as_str`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mapbox => "Mapbox",
            Self::Google => "Google",
        })
    }
}

/// The two monthly request caps, both configuration values. Usage counts
/// per provider: a cap bounds requests to one provider, and the same two
/// values govern Mapbox and Google alike. A cap that is not configured is
/// unlimited.
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    /// The fair-use cap: one relocation's requests to a provider in a
    /// month.
    pub relocation_monthly: Option<i64>,
    /// The operator's cap: every relocation's requests to a provider in a
    /// month.
    pub global_monthly: Option<i64>,
}

const RELOCATION_CAP_VAR: &str = "RELOCATION_MONTHLY_CAP";
const GLOBAL_CAP_VAR: &str = "GLOBAL_MONTHLY_CAP";

impl Caps {
    /// Reads `RELOCATION_MONTHLY_CAP` and `GLOBAL_MONTHLY_CAP`. An unset or
    /// blank variable leaves its cap unlimited; a set one must be a whole
    /// number of at least zero, so a mistyped cap surfaces as an error
    /// instead of silently lifting the limit.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            relocation_monthly: cap_from(
                env::var(RELOCATION_CAP_VAR).ok().as_deref(),
                RELOCATION_CAP_VAR,
            )?,
            global_monthly: cap_from(env::var(GLOBAL_CAP_VAR).ok().as_deref(), GLOBAL_CAP_VAR)?,
        })
    }
}

/// One configured cap: an unset or blank value is unlimited, anything else
/// must be a whole number of at least zero, and the error names the
/// variable so a mistyped value is findable.
fn cap_from(raw: Option<&str>, name: &str) -> Result<Option<i64>, String> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    text.parse::<i64>()
        .ok()
        .filter(|cap| *cap >= 0)
        .map(Some)
        .ok_or_else(|| format!("{name} must be a whole number of at least 0, not \"{text}\""))
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

/// Which monthly cap refused a request, with the numbers for its message.
#[derive(Debug)]
pub enum CapReached {
    /// The relocation's own fair-use cap for the provider.
    Relocation {
        provider: Provider,
        used: i64,
        cap: i64,
    },
    /// The operator's cap, across every relocation, for the provider.
    Global {
        provider: Provider,
        used: i64,
        cap: i64,
    },
}

impl fmt::Display for CapReached {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Relocation {
                provider,
                used,
                cap,
            } => write!(
                formatter,
                "monthly cap reached for this relocation: \
                 {used} {provider} requests this month, the cap is {cap}"
            ),
            Self::Global {
                provider,
                used,
                cap,
            } => write!(
                formatter,
                "global monthly cap reached: \
                 {used} {provider} requests this month, the cap is {cap}"
            ),
        }
    }
}

/// Why a cap check did not pass: the refusal itself, or the usage read
/// behind it failing.
#[derive(Debug)]
pub enum CapError {
    /// Reading this month's usage from `api_usage` failed.
    Usage(sqlx::Error),
    /// A configured cap is already reached; the request must not run.
    Reached(CapReached),
}

impl fmt::Display for CapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(error) => write!(formatter, "checking the monthly caps failed: {error}"),
            Self::Reached(reached) => write!(formatter, "{reached}"),
        }
    }
}

impl std::error::Error for CapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usage(error) => Some(error),
            Self::Reached(_) => None,
        }
    }
}

/// The gate every provider request passes before it is counted or sent:
/// refuses when a configured monthly cap for the provider is already
/// reached — the relocation's own fair-use cap first, then the global one.
/// A cap that is not configured never refuses.
pub async fn check_cap<'e, E>(
    executor: E,
    relocation_id: i64,
    provider: Provider,
    caps: Caps,
) -> Result<(), CapError>
where
    E: Executor<'e, Database = Sqlite>,
{
    if caps.relocation_monthly.is_none() && caps.global_monthly.is_none() {
        return Ok(());
    }
    // One read for both caps: this relocation's calls and every
    // relocation's, for this provider, in the month record_request writes
    // with SQLite's own clock.
    let (relocation_used, global_used): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(CASE WHEN relocation_id = ? THEN calls END), 0),
                COALESCE(SUM(calls), 0)
         FROM api_usage
         WHERE provider = ? AND month = strftime('%Y%m', 'now')",
    )
    .bind(relocation_id)
    .bind(provider.as_str())
    .fetch_one(executor)
    .await
    .map_err(CapError::Usage)?;
    if let Some(cap) = caps.relocation_monthly
        && relocation_used >= cap
    {
        return Err(CapError::Reached(CapReached::Relocation {
            provider,
            used: relocation_used,
            cap,
        }));
    }
    if let Some(cap) = caps.global_monthly
        && global_used >= cap
    {
        return Err(CapError::Reached(CapReached::Global {
            provider,
            used: global_used,
            cap,
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cap_from;

    #[test]
    fn cap_from_treats_an_unset_or_blank_value_as_unlimited() {
        assert_eq!(cap_from(None, "GLOBAL_MONTHLY_CAP"), Ok(None));
        assert_eq!(cap_from(Some(""), "GLOBAL_MONTHLY_CAP"), Ok(None));
        assert_eq!(cap_from(Some("  "), "GLOBAL_MONTHLY_CAP"), Ok(None));
    }

    #[test]
    fn cap_from_reads_a_whole_number_of_at_least_zero() {
        assert_eq!(cap_from(Some("100"), "GLOBAL_MONTHLY_CAP"), Ok(Some(100)));
        assert_eq!(cap_from(Some(" 7 "), "GLOBAL_MONTHLY_CAP"), Ok(Some(7)));
        assert_eq!(cap_from(Some("0"), "GLOBAL_MONTHLY_CAP"), Ok(Some(0)));
    }

    #[test]
    fn cap_from_names_the_variable_in_its_refusal_of_a_bad_value() {
        let error = cap_from(Some("-1"), "RELOCATION_MONTHLY_CAP").unwrap_err();
        assert_eq!(
            error,
            "RELOCATION_MONTHLY_CAP must be a whole number of at least 0, not \"-1\""
        );
        let error = cap_from(Some("many"), "GLOBAL_MONTHLY_CAP").unwrap_err();
        assert_eq!(
            error,
            "GLOBAL_MONTHLY_CAP must be a whole number of at least 0, not \"many\""
        );
    }
}
