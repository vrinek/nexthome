//! Evaluations: the travel-time table for one location — per place type
//! the three nearest accepted places, each with walking minutes from
//! point-in-polygon against the destination's stored isochrones and
//! bike, transit and car minutes from a batched Google route-matrix
//! request — computed on the first call, cached in `evaluations` after,
//! invalidated by the two writes that make it stale, and rendered as
//! the plan's text table. No test calls a live provider.

mod common;

use common::{FakeTransport, fixture, relocated_pool};
use nexthome::{
    api_usage::Caps,
    evaluations::{self, EvaluationRow, render},
    google_maps::GoogleMapsClient,
};
use serde_json::json;
use sqlx::SqlitePool;

const DISTANCE_MATRIX_URL: &str = "https://maps.googleapis.com/maps/api/distancematrix/json";

/// A Google Maps client on the fake transport with a test key and
/// unlimited caps, for the relocation the evaluation counts against.
fn google_client<'a>(
    transport: &'a FakeTransport,
    pool: &'a SqlitePool,
    relocation_id: i64,
) -> GoogleMapsClient<'a> {
    GoogleMapsClient {
        transport,
        pool,
        relocation_id,
        api_key: "test-key",
        caps: Caps::default(),
    }
}

/// The evaluation under test: the relocation's own Google client on
/// the fake transport.
async fn evaluate(
    pool: &SqlitePool,
    transport: &FakeTransport,
    relocation_id: i64,
    location_id: i64,
) -> Result<String, evaluations::EvaluationError> {
    evaluations::evaluate(
        pool,
        &google_client(transport, pool, relocation_id),
        location_id,
    )
    .await
}

/// One locations row at the given position in the given state, under
/// the given place type.
async fn insert_location(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type_id: Option<i64>,
    name: &str,
    lat: f64,
    lon: f64,
    state: &str,
) -> i64 {
    let member_id: i64 = sqlx::query_scalar("SELECT id FROM members ORDER BY id LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query_scalar(
        "INSERT INTO locations (relocation_id, place_type_id, member_id, name, lat, lon, state,
                                created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?,
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         RETURNING id",
    )
    .bind(relocation_id)
    .bind(place_type_id)
    .bind(member_id)
    .bind(name)
    .bind(lat)
    .bind(lon)
    .bind(state)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The id of one of the relocation's seeded place types, by name.
async fn place_type_id(pool: &SqlitePool, relocation_id: i64, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM place_types WHERE relocation_id = ? AND name = ?")
        .bind(relocation_id)
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The location's stored evaluation rows, parsed back.
async fn stored_rows(pool: &SqlitePool, location_id: i64) -> Vec<EvaluationRow> {
    let rows: String = sqlx::query_scalar("SELECT rows FROM evaluations WHERE location_id = ?")
        .bind(location_id)
        .fetch_one(pool)
        .await
        .unwrap();
    serde_json::from_str(&rows).unwrap()
}

/// The relocation's counted Google requests this month.
async fn google_calls(pool: &SqlitePool, relocation_id: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT calls FROM api_usage WHERE relocation_id = ? AND provider = 'google'",
    )
    .bind(relocation_id)
    .fetch_optional(pool)
    .await
    .unwrap()
    .unwrap_or(0)
}

/// The destinations parameter of every recorded Distance Matrix GET:
/// what the batching assertions read.
fn recorded_matrix_destinations(transport: &FakeTransport) -> Vec<String> {
    transport
        .recorded_gets()
        .iter()
        .filter(|get| get.url == DISTANCE_MATRIX_URL)
        .map(|get| {
            get.params()
                .iter()
                .find(|(name, _)| *name == "destinations")
                .expect("every matrix GET carries its destinations")
                .1
                .to_owned()
        })
        .collect()
}

/// Scripts one Distance Matrix answer: one origin, every destination
/// in order, `seconds` of travel each, or an unreachable pair when
/// None.
fn push_matrix(transport: &FakeTransport, seconds: &[Option<u64>]) {
    let elements = seconds
        .iter()
        .map(|seconds| match seconds {
            Some(seconds) => json!({"status": "OK", "duration": {"value": seconds}}),
            None => json!({"status": "ZERO_RESULTS"}),
        })
        .collect::<Vec<_>>();
    transport.push_ok_get(
        200,
        &json!({"rows": [{"elements": elements}], "status": "OK"}).to_string(),
    );
}

#[test]
fn the_table_renders_in_the_plan_s_format() {
    // The plan's own example table: the Work row renders although its
    // walk passes 30 minutes, because a mode reaches within them; the
    // Gym row shows "-" where a mode has no time; Martial arts, with no
    // place within 30 minutes by any mode, takes the "none within" line.
    let rows = vec![
        EvaluationRow {
            place_type: "Work".to_owned(),
            destination_location_id: 11,
            destination: "Alex office".to_owned(),
            walk: Some(45),
            bike: Some(18),
            transit: Some(22),
            car: Some(15),
        },
        EvaluationRow {
            place_type: "School".to_owned(),
            destination_location_id: 12,
            destination: "Grundschule Neumark".to_owned(),
            walk: Some(12),
            bike: Some(5),
            transit: Some(14),
            car: Some(7),
        },
        EvaluationRow {
            place_type: "Gym".to_owned(),
            destination_location_id: 13,
            destination: "Urban Apes".to_owned(),
            walk: Some(8),
            bike: Some(4),
            transit: None,
            car: Some(6),
        },
    ];
    let expected = [
        "Inselstraße 10, Mitte        walk  bike  transit  car",
        "Work: Alex office              45    18       22   15",
        "School: Grundschule Neumark    12     5       14    7",
        "Gym: Urban Apes                 8     4        -    6",
        "Martial arts: none within 30 min by any mode",
    ]
    .join("\n");

    assert_eq!(
        render(
            "Inselstraße 10, Mitte",
            &["Work", "School", "Gym", "Martial arts"],
            &rows,
        ),
        expected
    );
}

#[tokio::test]
async fn one_batched_matrix_request_per_mode_carries_every_destination() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let gym = place_type_id(&pool, relocation_id, "Gym").await;
    let apartment = place_type_id(&pool, relocation_id, "Apartment").await;
    // Two accepted gyms, the nearer one first on the wire; the flat in
    // the inbox, where every evaluation starts.
    let near = insert_location(
        &pool,
        relocation_id,
        Some(gym),
        "Urban Apes",
        52.5219,
        13.4139,
        "accepted",
    )
    .await;
    let far = insert_location(
        &pool,
        relocation_id,
        Some(gym),
        "Tempelhof gym",
        52.5122,
        13.4512,
        "accepted",
    )
    .await;
    let home = insert_location(
        &pool,
        relocation_id,
        Some(apartment),
        "Inselstraße 10, Mitte",
        52.52,
        13.405,
        "inbox",
    )
    .await;
    for _ in 0..3 {
        transport.push_ok_get(200, &fixture("google_distance_matrix.json"));
    }

    let table = evaluate(&pool, &transport, relocation_id, home)
        .await
        .unwrap();

    // The recorded answer carries 22 minutes to the first destination
    // and no route to the second: the nearer gym reaches, the farther
    // one has no mode within 30 minutes and leaves the table.
    let rows = stored_rows(&pool, home).await;
    assert_eq!(
        rows.len(),
        2,
        "both of the type's nearest places are stored, reachable or not"
    );
    assert_eq!(rows[0].place_type, "Gym");
    assert_eq!(rows[0].destination, "Urban Apes");
    assert_eq!(rows[0].destination_location_id, near);
    assert_eq!(rows[0].walk, None);
    assert_eq!(rows[0].bike, Some(22));
    assert_eq!(rows[0].transit, Some(22));
    assert_eq!(rows[0].car, Some(22));
    assert_eq!(rows[1].destination, "Tempelhof gym");
    assert_eq!(rows[1].destination_location_id, far);
    assert_eq!(rows[1].bike, None);
    assert_eq!(rows[1].transit, None);
    assert_eq!(rows[1].car, None);
    assert!(table.starts_with("Inselstraße 10, Mitte"));
    assert!(table.contains("Gym: Urban Apes"));
    assert!(!table.contains("Tempelhof gym"));
    assert!(table.contains("School: none within 30 min by any mode"));

    // One request per mode, every destination in it — not one request
    // per place.
    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 3);
    for (get, mode) in gets.iter().zip(["bicycling", "transit", "driving"]) {
        assert_eq!(get.url, DISTANCE_MATRIX_URL);
        assert_eq!(
            get.params(),
            vec![
                ("origins", "52.52,13.405"),
                ("destinations", "52.5219,13.4139|52.5122,13.4512"),
                ("mode", mode),
                ("key", "test-key"),
            ]
        );
    }
    assert_eq!(
        recorded_matrix_destinations(&transport),
        vec!["52.5219,13.4139|52.5122,13.4512"; 3]
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 3);
}

#[tokio::test]
async fn the_table_takes_the_three_nearest_accepted_places_of_a_type() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let school = place_type_id(&pool, relocation_id, "School").await;
    // The evaluated location is itself an accepted school, so the
    // nearest list must skip it; a rejected school nearer than the
    // accepted ones must never be taken; the fourth accepted school
    // falls past the three-place cap.
    let evaluated = insert_location(
        &pool,
        relocation_id,
        Some(school),
        "The school at home",
        52.52,
        13.405,
        "accepted",
    )
    .await;
    insert_location(
        &pool,
        relocation_id,
        Some(school),
        "Rejected school",
        52.52,
        13.41,
        "rejected",
    )
    .await;
    let first = insert_location(
        &pool,
        relocation_id,
        Some(school),
        "First school",
        52.52,
        13.42,
        "accepted",
    )
    .await;
    let second = insert_location(
        &pool,
        relocation_id,
        Some(school),
        "Second school",
        52.52,
        13.43,
        "accepted",
    )
    .await;
    let third = insert_location(
        &pool,
        relocation_id,
        Some(school),
        "Third school",
        52.52,
        13.44,
        "accepted",
    )
    .await;
    insert_location(
        &pool,
        relocation_id,
        Some(school),
        "Fourth school",
        52.52,
        13.45,
        "accepted",
    )
    .await;
    // The matrix answers 10, 20 and 30 minutes to the three that go.
    for _ in 0..3 {
        push_matrix(&transport, &[Some(600), Some(1200), Some(1800)]);
    }

    let table = evaluate(&pool, &transport, relocation_id, evaluated)
        .await
        .unwrap();

    let rows = stored_rows(&pool, evaluated).await;
    let names = rows
        .iter()
        .map(|row| row.destination.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["First school", "Second school", "Third school"]);
    let ids = rows
        .iter()
        .map(|row| row.destination_location_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, [first, second, third]);
    for (row, minutes) in rows.iter().zip([10, 20, 30]) {
        assert_eq!(row.place_type, "School");
        assert_eq!(row.bike, Some(minutes));
        assert_eq!(row.transit, Some(minutes));
        assert_eq!(row.car, Some(minutes));
    }
    assert!(table.contains("First school"));
    assert!(table.contains("Third school"));
    assert!(!table.contains("Fourth school"));
    assert!(!table.contains("Rejected school"));
    // Only the three that go reach the wire.
    assert_eq!(
        recorded_matrix_destinations(&transport),
        vec!["52.52,13.42|52.52,13.43|52.52,13.44"; 3]
    );
}

#[tokio::test]
async fn the_second_call_reads_the_stored_table_and_makes_no_request() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let gym = place_type_id(&pool, relocation_id, "Gym").await;
    let apartment = place_type_id(&pool, relocation_id, "Apartment").await;
    insert_location(
        &pool,
        relocation_id,
        Some(gym),
        "Urban Apes",
        52.5219,
        13.4139,
        "accepted",
    )
    .await;
    let home = insert_location(
        &pool,
        relocation_id,
        Some(apartment),
        "Inselstraße 10",
        52.52,
        13.405,
        "inbox",
    )
    .await;
    for _ in 0..3 {
        push_matrix(&transport, &[Some(900)]);
    }

    let first = evaluate(&pool, &transport, relocation_id, home)
        .await
        .unwrap();
    assert_eq!(transport.recorded_gets().len(), 3);
    assert_eq!(google_calls(&pool, relocation_id).await, 3);

    // Nothing is scripted onto the transport anymore: a second call
    // that went to Google would panic the fake rather than call out.
    let second = evaluate(&pool, &transport, relocation_id, home)
        .await
        .unwrap();

    assert_eq!(second, first, "the cached table is the computed one");
    assert_eq!(
        transport.recorded_gets().len(),
        3,
        "the cache answers without a request"
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 3);
    let evaluations_stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM evaluations WHERE location_id = ?")
            .bind(home)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(evaluations_stored, 1);
}

#[tokio::test]
async fn evaluating_fails_when_no_location_has_the_id() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();

    let error = evaluate(&pool, &transport, relocation_id, 999)
        .await
        .expect_err("there is no location to evaluate");

    assert_eq!(error.to_string(), "no location exists with id 999");
    assert!(transport.recorded_gets().is_empty());
}
