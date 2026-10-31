//! Isochrones over a real database: generating a location's walking
//! contours from Mapbox and skipping the ones it already has. No test
//! calls a live provider.

mod common;

use std::sync::Arc;

use common::{FakeTransport, fixture, mapbox_access, relocated_pool};
use nexthome::isochrones;
use sqlx::SqlitePool;

/// The minutes of every walking contour a location gets.
const ALL_MINUTES: [i64; 5] = [5, 10, 15, 20, 30];

/// The location's stored contours: the minutes with the geojson it
/// stores, smallest minutes first.
async fn stored_contours(pool: &SqlitePool, location_id: i64) -> Vec<(i64, String)> {
    sqlx::query_as("SELECT minutes, geojson FROM isochrones WHERE location_id = ? ORDER BY minutes")
        .bind(location_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The contour minutes the recorded GETs asked for, in the order the
/// generation made them.
fn fetched_minutes(transport: &FakeTransport) -> Vec<String> {
    transport
        .recorded_gets()
        .iter()
        .map(|get| {
            get.params()
                .iter()
                .find(|(name, _)| *name == "contours_minutes")
                .expect("every isochrone GET carries its contour minutes")
                .1
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn generation_fetches_and_stores_each_of_the_five_walking_contours() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = Arc::new(FakeTransport::new());
    for _ in ALL_MINUTES {
        transport.push_ok_get(200, &fixture("mapbox_isochrone.json"));
    }
    let location_id = common::insert_location(&pool, relocation_id, None, 52.5122, 13.405).await;
    let access = mapbox_access(transport.clone());

    isochrones::generate_for_location(&pool, &access, location_id)
        .await
        .unwrap();

    let contours = stored_contours(&pool, location_id).await;
    assert_eq!(
        contours
            .iter()
            .map(|(minutes, _)| *minutes)
            .collect::<Vec<_>>(),
        ALL_MINUTES.to_vec(),
        "the location ends up with one stored contour per configured minutes"
    );
    let fetched: serde_json::Value =
        serde_json::from_str(&fixture("mapbox_isochrone.json")).unwrap();
    for (minutes, geojson) in &contours {
        let stored: serde_json::Value = serde_json::from_str(geojson).unwrap();
        assert_eq!(
            &stored, &fetched,
            "the {minutes}-minute contour stores Mapbox's geojson verbatim"
        );
    }

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 5, "one request per contour");
    for (get, minutes) in gets.iter().zip(ALL_MINUTES) {
        assert_eq!(
            get.url,
            "https://api.mapbox.com/isochrone/v1/mapbox/walking/13.405,52.5122"
        );
        let minutes = minutes.to_string();
        assert_eq!(
            get.params(),
            vec![
                ("contours_minutes", minutes.as_str()),
                ("polygons", "true"),
                ("access_token", "test-token"),
            ]
        );
    }
    let calls: i64 = sqlx::query_scalar(
        "SELECT calls FROM api_usage WHERE relocation_id = ? AND provider = 'mapbox'",
    )
    .bind(relocation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(calls, 5, "every contour fetch counts against the caps");
}

#[tokio::test]
async fn generation_skips_the_contours_the_location_already_has() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let location_id = common::insert_location(&pool, relocation_id, None, 52.5122, 13.405).await;
    // The 5- and 20-minute contours were generated earlier; only the
    // other three are worth a request.
    for minutes in [5, 20] {
        sqlx::query("INSERT INTO isochrones (location_id, minutes, geojson) VALUES (?, ?, ?)")
            .bind(location_id)
            .bind(minutes)
            .bind(r#"{"type": "FeatureCollection", "features": []}"#)
            .execute(&pool)
            .await
            .unwrap();
    }
    let transport = Arc::new(FakeTransport::new());
    for _ in [10, 15, 30] {
        transport.push_ok_get(200, &fixture("mapbox_isochrone.json"));
    }
    let access = mapbox_access(transport.clone());

    isochrones::generate_for_location(&pool, &access, location_id)
        .await
        .unwrap();

    let contours = stored_contours(&pool, location_id).await;
    assert_eq!(
        contours
            .iter()
            .map(|(minutes, _)| *minutes)
            .collect::<Vec<_>>(),
        ALL_MINUTES.to_vec(),
        "the earlier contours stand beside the freshly generated ones"
    );
    let kept: serde_json::Value =
        serde_json::from_str(r#"{"type": "FeatureCollection", "features": []}"#).unwrap();
    let fetched: serde_json::Value =
        serde_json::from_str(&fixture("mapbox_isochrone.json")).unwrap();
    for (minutes, geojson) in &contours {
        let stored: serde_json::Value = serde_json::from_str(geojson).unwrap();
        let expected = if *minutes == 5 || *minutes == 20 {
            &kept
        } else {
            &fetched
        };
        assert_eq!(
            &stored, expected,
            "the {minutes}-minute contour is the one that belonged there"
        );
    }
    assert_eq!(
        fetched_minutes(&transport),
        vec!["10", "15", "30"],
        "only the missing contours are fetched"
    );
}

#[tokio::test]
async fn generation_fails_when_no_location_has_the_id() {
    let (_directory, pool, _relocation_id) = relocated_pool().await;
    let transport = Arc::new(FakeTransport::new());
    let access = mapbox_access(transport.clone());

    let error = isochrones::generate_for_location(&pool, &access, 999)
        .await
        .expect_err("there is no location to generate for");

    assert_eq!(error.to_string(), "no location exists with id 999");
    assert!(
        fetched_minutes(&transport).is_empty(),
        "nothing is fetched for an unknown location"
    );
}
