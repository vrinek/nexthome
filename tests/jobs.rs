mod common;

use common::migrated_pool;
use nexthome::jobs;

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
