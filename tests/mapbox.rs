mod common;

use common::{FakeTransport, fixture, relocated_pool};
use nexthome::{
    geo::Coordinates,
    mapbox::{MapboxClient, MapboxError},
};
use sqlx::SqlitePool;

/// A client on the fake transport with a test token, as production would
/// build one on `ReqwestTransport` with the real token.
fn client<'a>(
    transport: &'a FakeTransport,
    pool: &'a SqlitePool,
    relocation_id: i64,
) -> MapboxClient<'a> {
    MapboxClient {
        transport,
        pool,
        relocation_id,
        access_token: "test-token",
    }
}

/// The relocation's counted Mapbox requests this month.
async fn mapbox_calls(pool: &SqlitePool, relocation_id: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT calls FROM api_usage WHERE relocation_id = ? AND provider = 'mapbox'",
    )
    .bind(relocation_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .unwrap_or(0)
}

#[tokio::test]
async fn address_to_coordinates_biases_to_the_city_and_returns_the_first_feature_center() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("mapbox_geocode.json"));

    let coordinates = client(&transport, &pool, relocation_id)
        .address_to_coordinates(
            "Inselstraße 10, Berlin",
            Coordinates {
                lat: 52.52,
                lng: 13.405,
            },
        )
        .await
        .unwrap();

    // Mapbox ranks the Berlin hit first; its center is [lng, lat].
    assert_eq!(
        coordinates,
        Coordinates {
            lat: 52.51057,
            lng: 13.41057
        }
    );

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://api.mapbox.com/geocoding/v5/mapbox.places/Inselstra%C3%9Fe%2010%2C%20Berlin.json"
    );
    // The proximity bias takes the city position as lng,lat, Mapbox's order.
    assert_eq!(
        gets[0].params(),
        vec![
            ("proximity", "13.405,52.52"),
            ("access_token", "test-token")
        ]
    );
    assert_eq!(mapbox_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn address_to_coordinates_fails_when_geocoding_answers_no_feature() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, r#"{"type": "FeatureCollection", "features": []}"#);

    let error = client(&transport, &pool, relocation_id)
        .address_to_coordinates(
            "No Such Street 1, Berlin",
            Coordinates {
                lat: 52.52,
                lng: 13.405,
            },
        )
        .await
        .expect_err("an empty feature list leaves nothing to take the center from");

    assert!(
        matches!(error, MapboxError::NoGeocodeResults(_)),
        "expected a no-results error, got: {error}"
    );
    assert!(
        error.to_string().contains("No Such Street 1, Berlin"),
        "the message must name the address: {error}"
    );
}

#[tokio::test]
async fn address_to_coordinates_counts_one_request_across_transport_retries() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_failed_get("connection reset");
    transport.push_failed_get("connection reset");
    transport.push_ok_get(200, &fixture("mapbox_geocode.json"));

    let coordinates = client(&transport, &pool, relocation_id)
        .address_to_coordinates(
            "Inselstraße 10, Berlin",
            Coordinates {
                lat: 52.52,
                lng: 13.405,
            },
        )
        .await
        .unwrap();

    assert_eq!(coordinates.lat, 52.51057);
    assert_eq!(transport.recorded_gets().len(), 3);
    assert_eq!(
        mapbox_calls(&pool, relocation_id).await,
        1,
        "transport retries repeat one logical request, so they count once"
    );
}

#[tokio::test]
async fn address_to_coordinates_fails_when_the_transport_stays_broken() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    for _ in 0..3 {
        transport.push_failed_get("connection refused");
    }

    let error = client(&transport, &pool, relocation_id)
        .address_to_coordinates(
            "Inselstraße 10, Berlin",
            Coordinates {
                lat: 52.52,
                lng: 13.405,
            },
        )
        .await
        .expect_err("three transport failures exhaust the retries");

    assert!(
        matches!(error, MapboxError::Transport(_)),
        "expected a transport error, got: {error}"
    );
    assert_eq!(
        mapbox_calls(&pool, relocation_id).await,
        1,
        "the attempt was made and passed the cap check, so it counts"
    );
}

#[tokio::test]
async fn isochrone_fetches_the_walking_polygons_and_counts_the_request() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("mapbox_isochrone.json"));

    let geojson = client(&transport, &pool, relocation_id)
        .isochrone(
            Coordinates {
                lat: 52.5122,
                lng: 13.405,
            },
            5,
        )
        .await
        .unwrap();

    let expected: serde_json::Value =
        serde_json::from_str(&fixture("mapbox_isochrone.json")).unwrap();
    assert_eq!(geojson, expected);

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://api.mapbox.com/isochrone/v1/mapbox/walking/13.405,52.5122"
    );
    assert_eq!(
        gets[0].params(),
        vec![
            ("contours_minutes", "5"),
            ("polygons", "true"),
            ("access_token", "test-token"),
        ]
    );
    assert_eq!(mapbox_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn isochrone_retries_a_non_200_answer() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(500, "upstream overloaded");
    transport.push_ok_get(500, "upstream overloaded");
    transport.push_ok_get(200, &fixture("mapbox_isochrone.json"));

    let geojson = client(&transport, &pool, relocation_id)
        .isochrone(
            Coordinates {
                lat: 52.5122,
                lng: 13.405,
            },
            5,
        )
        .await
        .unwrap();

    assert_eq!(
        geojson["features"][0]["properties"]["contour"],
        serde_json::json!(5)
    );
    assert_eq!(transport.recorded_gets().len(), 3);
    assert_eq!(
        mapbox_calls(&pool, relocation_id).await,
        3,
        "each retried request is a fresh request and counts"
    );
}

#[tokio::test]
async fn isochrone_fails_after_three_non_200_answers() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    for _ in 0..3 {
        transport.push_ok_get(503, "service unavailable");
    }

    let error = client(&transport, &pool, relocation_id)
        .isochrone(
            Coordinates {
                lat: 52.5122,
                lng: 13.405,
            },
            5,
        )
        .await
        .expect_err("three non-200 answers in a row exhaust the retries");

    assert!(
        matches!(error, MapboxError::IsochroneFailed(503)),
        "expected a failed-isochrone error with the status, got: {error}"
    );
    assert_eq!(error.to_string(), "request to Mapbox failed: 503");
    assert_eq!(transport.recorded_gets().len(), 3);
    assert_eq!(mapbox_calls(&pool, relocation_id).await, 3);
}
