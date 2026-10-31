mod common;

use common::relocated_pool;
use nexthome::{
    api_usage::{self, CapError, CapReached, Caps, Provider},
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

#[tokio::test]
async fn check_cap_passes_under_both_caps() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let caps = Caps {
        relocation_monthly: Some(2),
        global_monthly: Some(3),
    };

    api_usage::check_cap(&pool, relocation_id, Provider::Mapbox, caps)
        .await
        .unwrap();

    api_usage::record_request(&pool, relocation_id, Provider::Mapbox)
        .await
        .unwrap();
    api_usage::check_cap(&pool, relocation_id, Provider::Mapbox, caps)
        .await
        .unwrap();
}

#[tokio::test]
async fn check_cap_refuses_when_the_relocation_cap_is_reached() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    for _ in 0..2 {
        api_usage::record_request(&pool, relocation_id, Provider::Mapbox)
            .await
            .unwrap();
    }

    let error = api_usage::check_cap(
        &pool,
        relocation_id,
        Provider::Mapbox,
        Caps {
            relocation_monthly: Some(2),
            global_monthly: Some(10),
        },
    )
    .await
    .expect_err("two recorded requests reach a cap of two");

    assert!(
        matches!(
            error,
            CapError::Reached(CapReached::Relocation {
                provider: Provider::Mapbox,
                used: 2,
                cap: 2
            })
        ),
        "expected the relocation cap refusal, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "monthly cap reached for this relocation: 2 Mapbox requests this month, the cap is 2"
    );
}

#[tokio::test]
async fn check_cap_refuses_when_the_global_cap_is_reached() {
    let (_directory, pool, berlin_id) = relocated_pool().await;
    let account_id: i64 = sqlx::query_scalar("SELECT account_id FROM relocations WHERE id = ?")
        .bind(berlin_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let vienna_id = relocations::create(&pool, account_id, "Vienna", 48.208, 16.373, 11.0)
        .await
        .unwrap();
    // Neither relocation is anywhere near its own cap; together they reach
    // the global one.
    for relocation_id in [berlin_id, vienna_id] {
        for _ in 0..2 {
            api_usage::record_request(&pool, relocation_id, Provider::Google)
                .await
                .unwrap();
        }
    }

    let error = api_usage::check_cap(
        &pool,
        berlin_id,
        Provider::Google,
        Caps {
            relocation_monthly: Some(5),
            global_monthly: Some(4),
        },
    )
    .await
    .expect_err("four requests across two relocations reach the global cap of four");

    assert!(
        matches!(
            error,
            CapError::Reached(CapReached::Global {
                provider: Provider::Google,
                used: 4,
                cap: 4
            })
        ),
        "expected the global cap refusal, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "global monthly cap reached: 4 Google requests this month, the cap is 4"
    );
}

#[tokio::test]
async fn check_cap_counts_each_provider_separately() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    for _ in 0..2 {
        api_usage::record_request(&pool, relocation_id, Provider::Mapbox)
            .await
            .unwrap();
    }
    let caps = Caps {
        relocation_monthly: Some(2),
        global_monthly: None,
    };

    // The relocation's Mapbox cap is reached while its Google usage is
    // untouched: the same two values govern each provider on its own.
    api_usage::check_cap(&pool, relocation_id, Provider::Google, caps)
        .await
        .unwrap();
    api_usage::check_cap(&pool, relocation_id, Provider::Mapbox, caps)
        .await
        .expect_err("the relocation's Mapbox calls alone reach its cap");
}

#[tokio::test]
async fn check_cap_is_unlimited_without_configured_caps() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    for _ in 0..5 {
        api_usage::record_request(&pool, relocation_id, Provider::Google)
            .await
            .unwrap();
    }

    api_usage::check_cap(&pool, relocation_id, Provider::Google, Caps::default())
        .await
        .unwrap();
}
