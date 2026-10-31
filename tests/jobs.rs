mod common;

use std::time::{Duration, Instant};

use common::{migrated_pool, relocated_pool};
use nexthome::jobs;
use sqlx::SqlitePool;

/// The job's attempts, last_error and done_at, as the tests read them
/// back.
async fn job_row(pool: &SqlitePool, job_id: i64) -> (i64, Option<String>, Option<String>) {
    sqlx::query_as("SELECT attempts, last_error, done_at FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The job's run_after beside SQLite's now and a ceiling the given
/// modifier puts beyond it, so a test can assert how far into the future
/// a backoff pushed the next attempt.
async fn run_after_window(
    pool: &SqlitePool,
    job_id: i64,
    ceiling: &str,
) -> (String, String, String) {
    sqlx::query_as(
        "SELECT run_after,
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)
         FROM jobs WHERE id = ?",
    )
    .bind(ceiling)
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Makes the job due again, as its backoff having passed.
async fn backoff_elapsed(pool: &SqlitePool, job_id: i64) {
    sqlx::query(
        "UPDATE jobs SET run_after = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 minute')
         WHERE id = ?",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn claim_hands_out_each_due_job_once_oldest_first() {
    let (_directory, pool) = migrated_pool().await;

    let first = jobs::enqueue_archive_expired(&pool).await.unwrap();
    let second = jobs::enqueue_archive_expired(&pool).await.unwrap();

    let claimed = jobs::claim(&pool)
        .await
        .unwrap()
        .expect("the oldest due job is claimable");
    assert_eq!(
        (claimed.id, claimed.kind.as_str(), claimed.attempts),
        (first, "archive_expired", 1),
        "the claim takes the oldest due job and counts its first attempt"
    );

    // The claim leases the job it hands out, so the next claim gets the
    // next job rather than the same one again.
    let next = jobs::claim(&pool)
        .await
        .unwrap()
        .expect("the second job is claimable");
    assert_eq!((next.id, next.attempts), (second, 1));

    assert!(
        jobs::claim(&pool).await.unwrap().is_none(),
        "both jobs are leased, so nothing is due"
    );
}

#[tokio::test]
async fn claim_skips_jobs_that_are_not_due_done_or_exhausted() {
    let (_directory, pool) = migrated_pool().await;

    // One job not due until an hour from now, one done, and one out of
    // attempts.
    sqlx::query(
        "INSERT INTO jobs (kind, payload, run_after, attempts, done_at) VALUES
         ('populate', '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+1 hour'), 0, NULL),
         ('populate', '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 minute'), 0,
          strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
         ('populate', '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 minute'), 5, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let due = jobs::enqueue_isochrones(&pool, 1).await.unwrap();

    let claimed = jobs::claim(&pool)
        .await
        .unwrap()
        .expect("only the due, unfinished, unexhausted job is claimable");
    assert_eq!((claimed.id, claimed.kind.as_str()), (due, "isochrones"));
}

#[tokio::test]
async fn a_failing_job_records_its_error_and_waits_out_its_backoff() {
    let (_directory, pool, _relocation_id) = relocated_pool().await;

    let job_id = jobs::enqueue_isochrones(&pool, 999).await.unwrap();
    assert!(jobs::process_next(&pool).await.unwrap(), "the job is due");

    let (attempts, last_error, done_at) = job_row(&pool, job_id).await;
    assert_eq!(attempts, 1);
    assert_eq!(
        last_error.as_deref(),
        Some("no location exists with id 999")
    );
    assert!(done_at.is_none(), "a failed job is not done");

    // The backoff pushed the retry ten seconds out: nothing is claimable
    // until then.
    let (run_after, now, ceiling) = run_after_window(&pool, job_id, "+12 seconds").await;
    assert!(
        now < run_after && run_after <= ceiling,
        "run_after {run_after} must sit ~10 s past {now}"
    );
    assert!(
        !jobs::process_next(&pool).await.unwrap(),
        "the backed-off job is not claimable yet"
    );
}

#[tokio::test]
async fn a_failing_job_is_retried_with_backoff_up_to_five_attempts() {
    let (_directory, pool, relocation_id) = relocated_pool().await;

    let job_id = jobs::enqueue_populate(&pool, relocation_id, 999, 52.52, 13.405)
        .await
        .unwrap();
    let expected_error = format!("no place type 999 exists in relocation {relocation_id}");

    // The waits the failures below must pin: ten seconds after the first
    // attempt, doubling each time.
    let expected_backoff = [10, 20, 40, 80];
    for attempt in 1i64..=5 {
        assert!(
            jobs::process_next(&pool).await.unwrap(),
            "attempt {attempt} must find the job due"
        );

        let (attempts, last_error, done_at) = job_row(&pool, job_id).await;
        assert_eq!(attempts, attempt, "the claim counts each attempt");
        assert_eq!(last_error.as_deref(), Some(expected_error.as_str()));
        assert!(done_at.is_none(), "a failed job is not done");

        if attempt < 5 {
            let wait = expected_backoff[(attempt - 1) as usize];
            let (run_after, now, ceiling) =
                run_after_window(&pool, job_id, &format!("+{} seconds", wait + 2)).await;
            assert!(
                now < run_after && run_after <= ceiling,
                "run_after {run_after} must sit ~{wait} s past {now}"
            );
            backoff_elapsed(&pool, job_id).await;
        }
    }

    // The five attempts are spent; even with run_after in the past, the
    // job is never claimed again.
    backoff_elapsed(&pool, job_id).await;
    assert!(
        !jobs::process_next(&pool).await.unwrap(),
        "an exhausted job stays unclaimed, its last error kept"
    );
}

#[tokio::test]
async fn a_job_that_succeeds_is_marked_done_and_not_claimed_again() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let member_id: i64 = sqlx::query_scalar("SELECT id FROM members")
        .fetch_one(&pool)
        .await
        .unwrap();
    let location_id: i64 = sqlx::query_scalar(
        "INSERT INTO locations (relocation_id, place_type_id, member_id, name, lat, lon,
                                created_at, updated_at)
         VALUES (?, NULL, ?, 'Alex office', 52.52, 13.405,
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         RETURNING id",
    )
    .bind(relocation_id)
    .bind(member_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let job_id = jobs::enqueue_isochrones(&pool, location_id).await.unwrap();
    assert!(jobs::process_next(&pool).await.unwrap(), "the job is due");

    let (attempts, last_error, done_at) = job_row(&pool, job_id).await;
    assert_eq!(attempts, 1);
    assert!(last_error.is_none());
    let done_at = done_at.expect("the successful job is done");
    assert_eq!(done_at.len(), 20);
    assert!(done_at.ends_with('Z'));

    assert!(
        !jobs::process_next(&pool).await.unwrap(),
        "a done job is not claimable again"
    );
}

#[tokio::test]
async fn the_archive_expired_job_runs_the_archiver() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    // Paid seven months ago, so expired a month ago.
    sqlx::query(
        "UPDATE relocations
         SET paid_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-7 months'),
             expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 month')
         WHERE id = ?",
    )
    .bind(relocation_id)
    .execute(&pool)
    .await
    .unwrap();

    let job_id = jobs::enqueue_archive_expired(&pool).await.unwrap();
    assert!(jobs::process_next(&pool).await.unwrap(), "the job is due");

    let (_, _, done_at) = job_row(&pool, job_id).await;
    assert!(done_at.is_some(), "the archive job completed");
    let archived_at: Option<String> =
        sqlx::query_scalar("SELECT archived_at FROM relocations WHERE id = ?")
            .bind(relocation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        archived_at.is_some(),
        "the job archived the expired relocation"
    );
}

#[tokio::test]
async fn the_populate_job_completes_for_a_place_type_in_the_relocation() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let place_type_id: i64 = sqlx::query_scalar(
        "SELECT id FROM place_types WHERE relocation_id = ? AND name = 'School'",
    )
    .bind(relocation_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let job_id = jobs::enqueue_populate(&pool, relocation_id, place_type_id, 52.52, 13.405)
        .await
        .unwrap();
    assert!(jobs::process_next(&pool).await.unwrap(), "the job is due");

    let (attempts, last_error, done_at) = job_row(&pool, job_id).await;
    assert_eq!(attempts, 1);
    assert!(last_error.is_none());
    assert!(done_at.is_some(), "the populate job completed");
}

#[tokio::test]
async fn an_unknown_kind_or_a_bad_payload_fails_the_job_with_the_reason() {
    let (_directory, pool) = migrated_pool().await;
    sqlx::query(
        r#"INSERT INTO jobs (kind, payload, run_after) VALUES
           ('milk_cows', '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
           ('isochrones', '{"location_id": "one"}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
           ('isochrones', 'not json', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    for _ in 0..3 {
        assert!(
            jobs::process_next(&pool).await.unwrap(),
            "each bad job is due until it fails"
        );
    }

    let errors: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, last_error FROM jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        errors[0],
        ("milk_cows".into(), "unknown job kind: milk_cows".into())
    );
    assert_eq!(
        errors[1],
        (
            "isochrones".into(),
            "malformed payload: the location_id field must be a whole number".into()
        )
    );
    assert!(
        errors[2]
            .1
            .starts_with("malformed payload: payload is not valid JSON")
    );
}

#[tokio::test]
async fn the_loop_runs_due_jobs_to_completion() {
    let (_directory, pool) = migrated_pool().await;
    jobs::enqueue_archive_expired(&pool).await.unwrap();
    jobs::enqueue_archive_expired(&pool).await.unwrap();

    tokio::spawn(jobs::run(pool.clone()));

    // The loop polls every second, so both jobs should be done well
    // inside ten.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let done: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE done_at IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
        if done == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the loop did not finish both jobs within ten seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
