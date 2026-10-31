//! Background jobs over the `jobs` table: the enqueue functions, the
//! atomic claim the loop claims through — one job at a time — and the
//! three job kinds it runs.

use serde_json::{Value, json};
use sqlx::SqlitePool;

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
