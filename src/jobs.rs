//! Background jobs over the `jobs` table: the enqueue functions, the
//! atomic claim the loop claims through — one job at a time — and the
//! three job kinds it runs.

use std::{fmt, time::Duration};

use serde_json::{Value, json};
use sqlx::SqlitePool;

use crate::relocations;

/// The `jobs.kind` the isochrones job carries: walking isochrones for one
/// location.
const KIND_ISOCHRONES: &str = "isochrones";
/// The `jobs.kind` the populate job carries: nearby places for one place
/// type around a center.
const KIND_POPULATE: &str = "populate";
/// The `jobs.kind` the archiver job carries: relocations past expiry turn
/// read-only.
const KIND_ARCHIVE_EXPIRED: &str = "archive_expired";

/// How many times a job is attempted before the claim stops taking it:
/// the first attempt plus four retries.
const MAX_ATTEMPTS: i64 = 5;

/// How long a claim holds the job it hands out. A claimed job is not due
/// again until the lease passes, so a run that dies mid-job is retried
/// after the lease rather than straight away, and no second claimer can
/// take a job while it runs.
const CLAIM_LEASE_SECONDS: i64 = 600;

/// The seconds to wait before the next attempt, by the attempt that just
/// failed: ten seconds after the first, doubling each time.
const RETRY_BACKOFF_SECONDS: [i64; 4] = [10, 20, 40, 80];

/// How long the loop waits between polls when no job is due, so a fresh
/// job starts within a second of becoming due.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Why a job run failed; the message lands in the job's `last_error`.
#[derive(Debug)]
enum JobError {
    /// The row's kind is none the loop knows.
    UnknownKind(String),
    /// The payload is not the JSON its kind carries.
    Payload(String),
    /// No location has the id the isochrones job names.
    MissingLocation(i64),
    /// No place type has the id the populate job names in its relocation.
    MissingPlaceType {
        relocation_id: i64,
        place_type_id: i64,
    },
    /// A database operation behind the job failed.
    Database(sqlx::Error),
}

impl From<sqlx::Error> for JobError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(kind) => write!(formatter, "unknown job kind: {kind}"),
            Self::Payload(detail) => write!(formatter, "malformed payload: {detail}"),
            Self::MissingLocation(location_id) => {
                write!(formatter, "no location exists with id {location_id}")
            }
            Self::MissingPlaceType {
                relocation_id,
                place_type_id,
            } => write!(
                formatter,
                "no place type {place_type_id} exists in relocation {relocation_id}"
            ),
            Self::Database(error) => write!(formatter, "database operation failed: {error}"),
        }
    }
}

impl std::error::Error for JobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

/// A claimed job: the row as the claim returned it, with `attempts`
/// already counting the attempt that is about to run.
#[derive(Debug)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    pub attempts: i64,
}

/// Enqueues the isochrones job for one location.
pub async fn enqueue_isochrones(pool: &SqlitePool, location_id: i64) -> Result<i64, sqlx::Error> {
    enqueue(
        pool,
        KIND_ISOCHRONES,
        &json!({ "location_id": location_id }),
    )
    .await
}

/// Enqueues the populate job for one place type around a center.
pub async fn enqueue_populate(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type_id: i64,
    lat: f64,
    lon: f64,
) -> Result<i64, sqlx::Error> {
    enqueue(
        pool,
        KIND_POPULATE,
        &json!({
            "relocation_id": relocation_id,
            "place_type_id": place_type_id,
            "lat": lat,
            "lon": lon,
        }),
    )
    .await
}

/// Enqueues the archive_expired job, which archives every expired
/// relocation.
pub async fn enqueue_archive_expired(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    enqueue(pool, KIND_ARCHIVE_EXPIRED, &json!({})).await
}

/// Inserts the job as due now and returns its id.
async fn enqueue(pool: &SqlitePool, kind: &str, payload: &Value) -> Result<i64, sqlx::Error> {
    let payload = serde_json::to_string(payload).expect("a JSON value serializes");
    sqlx::query_scalar(
        "INSERT INTO jobs (kind, payload, run_after)
         VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .fetch_one(pool)
    .await
}

/// Claims the oldest due job, if one is: one UPDATE that picks the row,
/// counts the attempt, and leases the job for `CLAIM_LEASE_SECONDS` — all
/// in the single statement, which SQLite's one writer at a time keeps
/// atomic. A job is due when it is not done, still has attempts left, and
/// its `run_after` has passed; None when no job is due.
pub async fn claim(pool: &SqlitePool) -> Result<Option<Job>, sqlx::Error> {
    let leased = sqlx::query_as::<_, (i64, String, String, i64)>(
        "UPDATE jobs
         SET attempts = attempts + 1,
             run_after = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)
         WHERE id = (
             SELECT id FROM jobs
             WHERE done_at IS NULL
               AND attempts < ?
               AND run_after <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             ORDER BY id
             LIMIT 1
         )
         RETURNING id, kind, payload, attempts",
    )
    .bind(format!("+{CLAIM_LEASE_SECONDS} seconds"))
    .bind(MAX_ATTEMPTS)
    .fetch_optional(pool)
    .await?;
    Ok(leased.map(|(id, kind, payload, attempts)| Job {
        id,
        kind,
        payload,
        attempts,
    }))
}

/// Runs the next due job, if one is: claims it, executes it, and records
/// the outcome — `done_at` on success, `last_error` and the next backoff
/// on failure. A job that fails five times keeps its last error and is
/// never claimed again. Returns whether a job ran.
pub async fn process_next(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let Some(job) = claim(pool).await? else {
        return Ok(false);
    };
    match execute(pool, &job).await {
        Ok(()) => {
            sqlx::query(
                "UPDATE jobs SET done_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
            )
            .bind(job.id)
            .execute(pool)
            .await?;
        }
        Err(error) => {
            tracing::warn!(job_id = job.id, attempt = job.attempts, %error, "job failed");
            if job.attempts < MAX_ATTEMPTS {
                let backoff = RETRY_BACKOFF_SECONDS[(job.attempts - 1) as usize];
                sqlx::query(
                    "UPDATE jobs
                     SET last_error = ?,
                         run_after = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)
                     WHERE id = ?",
                )
                .bind(error.to_string())
                .bind(format!("+{backoff} seconds"))
                .bind(job.id)
                .execute(pool)
                .await?;
            } else {
                // The last attempt failed; the claim's attempts filter
                // keeps the job unclaimed from here on.
                sqlx::query("UPDATE jobs SET last_error = ? WHERE id = ?")
                    .bind(error.to_string())
                    .bind(job.id)
                    .execute(pool)
                    .await?;
            }
        }
    }
    Ok(true)
}

/// Runs one claimed job by its kind.
async fn execute(pool: &SqlitePool, job: &Job) -> Result<(), JobError> {
    let payload: Value = serde_json::from_str(&job.payload)
        .map_err(|error| JobError::Payload(format!("payload is not valid JSON: {error}")))?;
    match job.kind.as_str() {
        KIND_ISOCHRONES => isochrones(pool, &payload).await,
        KIND_POPULATE => populate(pool, &payload).await,
        KIND_ARCHIVE_EXPIRED => {
            relocations::archive_expired(pool).await?;
            Ok(())
        }
        unknown => Err(JobError::UnknownKind(unknown.to_owned())),
    }
}

/// The isochrones job: walking isochrones for one location. The contour
/// generation is PLAN.md task 9's; until it lands, the job only checks
/// its referent — a location that no longer exists fails the job, and
/// one that exists completes it.
async fn isochrones(pool: &SqlitePool, payload: &Value) -> Result<(), JobError> {
    let location_id = integer_field(payload, "location_id")?;
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM locations WHERE id = ?")
        .bind(location_id)
        .fetch_optional(pool)
        .await?;
    if found.is_none() {
        return Err(JobError::MissingLocation(location_id));
    }
    Ok(())
}

/// The populate job: fills a place type with nearby places around a
/// center. Creating those locations is PLAN.md task 8's; until it lands,
/// the job only checks its referent — the place type must exist in the
/// relocation.
async fn populate(pool: &SqlitePool, payload: &Value) -> Result<(), JobError> {
    let relocation_id = integer_field(payload, "relocation_id")?;
    let place_type_id = integer_field(payload, "place_type_id")?;
    // The center fields are checked now and used once task 8's location
    // writes land; both must be numbers.
    let _lat = number_field(payload, "lat")?;
    let _lon = number_field(payload, "lon")?;
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM place_types WHERE id = ? AND relocation_id = ?")
            .bind(place_type_id)
            .bind(relocation_id)
            .fetch_optional(pool)
            .await?;
    if found.is_none() {
        return Err(JobError::MissingPlaceType {
            relocation_id,
            place_type_id,
        });
    }
    Ok(())
}

/// The payload's integer field, or the payload error naming it.
fn integer_field(payload: &Value, name: &str) -> Result<i64, JobError> {
    payload[name]
        .as_i64()
        .ok_or_else(|| JobError::Payload(format!("the {name} field must be a whole number")))
}

/// The payload's number field, or the payload error naming it.
fn number_field(payload: &Value, name: &str) -> Result<f64, JobError> {
    payload[name]
        .as_f64()
        .ok_or_else(|| JobError::Payload(format!("the {name} field must be a number")))
}

/// The background loop: one job at a time — claim, run, record — sleeping
/// a poll interval whenever no job is due or the database errs. The plan
/// keeps one process the only writer, and this loop is the only claimer
/// in it, so jobs never run concurrently. Runs forever; the server
/// spawns it at startup.
pub async fn run(pool: SqlitePool) {
    loop {
        match process_next(&pool).await {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(error) => {
                tracing::warn!(%error, "looking for the next job failed");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}
