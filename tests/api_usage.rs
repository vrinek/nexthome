mod common;

use common::relocated_pool;
use nexthome::{
    api_usage::{self, Provider},
    relocations,
};

#[tokio::test]
async fn record_request_counts_each_provider_in_the_current_month() {
    let (_directory, pool, relocation_id) = relocated_pool().await;

    for _ in 0..3 {
        api_usage::record_request(&pool, relocation_id, Provider::Mapbox)
            .await
            .unwrap();
    }
    api_usage::record_request(&pool, relocation_id, Provider::Google)
        .await
        .unwrap();

    // The month comes from SQLite's strftime, the same clock that wrote the
    // row, so the assertion cannot drift across a month boundary.
    let month: i64 = sqlx::query_scalar("SELECT CAST(strftime('%Y%m', 'now') AS INTEGER)")
        .fetch_one(&pool)
        .await
        .unwrap();
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT provider, month, calls FROM api_usage WHERE relocation_id = ? ORDER BY provider",
    )
    .bind(relocation_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![("google".into(), month, 1), ("mapbox".into(), month, 3)],
        "three Mapbox requests and one Google request, one row per provider"
    );
}

#[tokio::test]
async fn record_request_counts_relocations_separately() {
    let (_directory, pool, berlin_id) = relocated_pool().await;
    let account_id: i64 = sqlx::query_scalar("SELECT account_id FROM relocations WHERE id = ?")
        .bind(berlin_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let vienna_id = relocations::create(&pool, account_id, "Vienna", 48.208, 16.373, 11.0)
        .await
        .unwrap();

    api_usage::record_request(&pool, berlin_id, Provider::Google)
        .await
        .unwrap();
    api_usage::record_request(&pool, vienna_id, Provider::Google)
        .await
        .unwrap();
    api_usage::record_request(&pool, vienna_id, Provider::Google)
        .await
        .unwrap();

    let calls: Vec<(i64, i64)> =
        sqlx::query_as("SELECT relocation_id, calls FROM api_usage ORDER BY relocation_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        calls,
        vec![(berlin_id, 1), (vienna_id, 2)],
        "the upsert must key on the relocation, not just the provider and month"
    );
}

#[tokio::test]
async fn record_request_rejects_an_unknown_relocation() {
    let (_directory, pool, _relocation_id) = relocated_pool().await;

    api_usage::record_request(&pool, 999, Provider::Mapbox)
        .await
        .expect_err("no relocation has id 999, the foreign key must reject it");
}
