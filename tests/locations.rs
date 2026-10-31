//! Locations over a real database: the state enum, the three create paths
//! (address, coordinates, Google Maps link), duplicate rejection, update
//! and delete with their job and evaluation side effects, the archive
//! guard, and listing with filters. No test calls a live provider.

mod common;

use std::str::FromStr;

use common::{FakeTransport, fixture, relocated_pool};
use nexthome::{
    accounts,
    api_usage::Caps,
    geo::Coordinates,
    google_maps::GoogleMapsClient,
    locations::{self, LocationError, NewLocation, Placement, State},
    mapbox::MapboxClient,
    relocations,
};
use sqlx::SqlitePool;

/// A shared Maps short link, as a mover pastes out of the Maps app.
const SHORT_LINK: &str = "https://maps.app.goo.gl/Rk5HAHR3rdiQgvJeA";
/// A second shared link to the same place, for the duplicate test.
const SHORT_LINK_WITHOUT_A_CENTRE: &str = "https://maps.app.goo.gl/AnotherLink";
/// The place url the short link expands to: the school, with its map
/// centre and zoom.
const PLACE_URL: &str =
    "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/@52.5267634,13.407897,16.17z";
/// The same place url without a map centre, so the place search runs
/// unbiassed.
const PLACE_URL_WITHOUT_A_CENTRE: &str =
    "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/data=!4m2";
/// The Places API v1 details for the place the test links resolve to.
const PLACE_DETAILS: &str = r#"{
    "id": "ChIJtestplaceid",
    "displayName": { "text": "Berlin Cosmopolitan School", "languageCode": "en" },
    "location": { "latitude": 52.5267634, "longitude": 13.407897 },
    "websiteUri": "https://www.berlin-cosmopolitan.de/"
}"#;
/// The same details with no display name, so the caller's name stands.
const PLACE_DETAILS_WITHOUT_A_NAME: &str = r#"{
    "id": "ChIJtestplaceid",
    "location": { "latitude": 52.5267634, "longitude": 13.407897 }
}"#;
/// The same details with no position, so no location can be created.
const PLACE_DETAILS_WITHOUT_A_POSITION: &str = r#"{
    "id": "ChIJtestplaceid",
    "displayName": { "text": "Berlin Cosmopolitan School", "languageCode": "en" }
}"#;

/// A Mapbox client on the fake transport with a test token and unlimited
/// caps, for one relocation.
fn mapbox_client<'a>(
    transport: &'a FakeTransport,
    pool: &'a SqlitePool,
    relocation_id: i64,
) -> MapboxClient<'a> {
    MapboxClient {
        transport,
        pool,
        relocation_id,
        access_token: "test-token",
        caps: Caps::default(),
    }
}

/// A Google Maps client on the fake transport with a test key and
/// unlimited caps, for one relocation.
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

/// The add under test: the relocation's own clients on the fake
/// transport, writing as the given member.
async fn add_location(
    pool: &SqlitePool,
    transport: &FakeTransport,
    relocation_id: i64,
    member_id: i64,
    new_location: NewLocation<'_>,
) -> Result<i64, LocationError> {
    locations::add(
        pool,
        relocation_id,
        member_id,
        &mapbox_client(transport, pool, relocation_id),
        &google_client(transport, pool, relocation_id),
        new_location,
    )
    .await
}

/// The account's first member: the one whose stamp the tests expect on
/// every created location.
async fn first_member_id(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT id FROM members ORDER BY id LIMIT 1")
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

/// How many isochrones jobs stand queued for the location.
async fn isochrones_job_count(pool: &SqlitePool, location_id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'isochrones' AND payload = ?")
        .bind(format!("{{\"location_id\":{location_id}}}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// How many locations the relocation tracks.
async fn location_count(pool: &SqlitePool, relocation_id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM locations WHERE relocation_id = ?")
        .bind(relocation_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One locations row as the tests read it back, everything but the
/// timestamps: id, place_type_id, member_id, name, lat, lon, state, note,
/// source_url, external_identifier, google_maps_payload.
type LocationRow = (
    i64,
    Option<i64>,
    i64,
    String,
    f64,
    f64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn location_row(pool: &SqlitePool, location_id: i64) -> LocationRow {
    sqlx::query_as(
        "SELECT id, place_type_id, member_id, name, lat, lon, state, note, source_url,
                external_identifier, google_maps_payload
         FROM locations WHERE id = ?",
    )
    .bind(location_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[test]
fn every_state_round_trips_through_its_stored_name() {
    let states = [
        State::Inbox,
        State::Rejected,
        State::Accepted,
        State::Contacted,
        State::Waitlisted,
        State::Booked,
    ];
    for state in states {
        let text = state.as_str();
        assert_eq!(
            State::from_str(text).ok(),
            Some(state),
            "{text} must parse back into the state it names"
        );
    }
}

#[test]
fn every_state_carries_the_marker_colour_the_rails_app_gave_it() {
    let colours = [
        (State::Inbox, "#1982c4"),
        (State::Rejected, "#ff595e"),
        (State::Accepted, "#8ac926"),
        (State::Contacted, "#ffca3a"),
        (State::Waitlisted, "#f77f00"),
        (State::Booked, "#6a4c93"),
    ];
    for (state, colour) in colours {
        assert_eq!(state.color(), colour);
    }
}

#[test]
fn parsing_refuses_a_name_no_state_has() {
    let error = State::from_str("bookmarked").unwrap_err();
    assert!(
        matches!(error, LocationError::UnknownState(ref text) if text == "bookmarked"),
        "expected an unknown-state error, got: {error}"
    );
    assert_eq!(error.to_string(), "bookmarked is not a location state");
}

#[tokio::test]
async fn adding_from_an_address_geocodes_through_mapbox_biased_to_the_city() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("mapbox_geocode.json"));
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let location_id = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "Home",
            place_type_id: school_id,
            note: Some("third floor walk-up"),
            source_url: None,
            placement: Placement::Address("Inselstraße 10, Berlin"),
        },
    )
    .await
    .unwrap();

    let row = location_row(&pool, location_id).await;
    let (_, place_type_id, member, name, lat, lon, state, note, source_url, _, _) = row;
    assert_eq!(place_type_id, Some(school_id));
    assert_eq!(member, member_id);
    assert_eq!(name, "Home");
    assert_eq!((lat, lon), (52.51057, 13.41057));
    assert_eq!(state, "inbox");
    assert_eq!(note.as_deref(), Some("third floor walk-up"));
    assert!(source_url.is_none());

    // Mapbox geocoded alone, biased to the city as lng,lat.
    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://api.mapbox.com/geocoding/v5/mapbox.places/Inselstra%C3%9Fe%2010%2C%20Berlin.json"
    );
    assert_eq!(
        gets[0].params(),
        vec![
            ("proximity", "13.405,52.52"),
            ("access_token", "test-token")
        ]
    );

    assert_eq!(isochrones_job_count(&pool, location_id).await, 1);
}

#[tokio::test]
async fn adding_from_an_address_falls_back_to_google_when_mapbox_answers_far_from_the_city() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    // Mapbox ranks a Munich address first: far outside Berlin.
    transport.push_ok_get(
        200,
        r#"{"type": "FeatureCollection", "features": [{"center": [11.575, 48.137]}]}"#,
    );
    transport.push_ok_get(200, &fixture("google_geocode.json"));
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let location_id = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "Home",
            place_type_id: school_id,
            note: None,
            source_url: None,
            placement: Placement::Address("Sendlinger Straße 1, Munich"),
        },
    )
    .await
    .unwrap();

    let (_, _, _, name, lat, lon, ..) = location_row(&pool, location_id).await;
    assert_eq!(name, "Home");
    assert_eq!((lat, lon), (52.5106, 13.4106), "Google's answer is stored");

    let gets = transport.recorded_gets();
    assert_eq!(
        gets.len(),
        2,
        "Mapbox answered too far, so Google geocodes next"
    );
    assert_eq!(
        gets[1].url,
        "https://maps.googleapis.com/maps/api/geocode/json"
    );
    assert_eq!(
        gets[1].params(),
        vec![
            ("address", "Sendlinger Straße 1, Munich"),
            ("key", "test-key")
        ]
    );

    assert_eq!(isochrones_job_count(&pool, location_id).await, 1);
}

#[tokio::test]
async fn adding_from_an_address_is_refused_when_google_answers_far_from_the_city_too() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(
        200,
        r#"{"type": "FeatureCollection", "features": [{"center": [11.575, 48.137]}]}"#,
    );
    transport.push_ok_get(
        200,
        r#"{"results": [{"geometry": {"location": {"lat": 48.137, "lng": 11.575}}}],
            "status": "OK"}"#,
    );
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "Home",
            place_type_id: school_id,
            note: None,
            source_url: None,
            placement: Placement::Address("Sendlinger Straße 1, Munich"),
        },
    )
    .await
    .expect_err("neither provider found the address near the city");

    assert!(
        matches!(
            &error,
            LocationError::TooFarFromCity { address, city }
                if address == "Sendlinger Straße 1, Munich" && city == "Berlin"
        ),
        "expected a too-far refusal naming the address and the city, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "no geocode for Sendlinger Straße 1, Munich within one degree of Berlin"
    );
    assert_eq!(location_count(&pool, relocation_id).await, 0);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "a refused write enqueues nothing");
}

#[tokio::test]
async fn adding_from_coordinates_needs_no_provider_call() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    // Nothing is scripted: any provider request fails the test.
    let transport = FakeTransport::new();
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let location_id = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "Großzügige Wohnung",
            place_type_id: school_id,
            note: None,
            source_url: Some("https://example.com/listing/686b"),
            placement: Placement::Coordinates(Coordinates {
                lat: 52.5340157,
                lng: 13.4120931,
            }),
        },
    )
    .await
    .unwrap();

    let row = location_row(&pool, location_id).await;
    let (_, _, _, name, lat, lon, state, _, source_url, external_id, payload) = row;
    assert_eq!(name, "Großzügige Wohnung");
    assert_eq!((lat, lon), (52.5340157, 13.4120931));
    assert_eq!(state, "inbox");
    assert_eq!(
        source_url.as_deref(),
        Some("https://example.com/listing/686b")
    );
    assert!(external_id.is_none());
    assert!(payload.is_none());

    assert!(transport.recorded_gets().is_empty());
    assert!(transport.recorded_heads().is_empty());
    assert_eq!(isochrones_job_count(&pool, location_id).await, 1);
}

#[tokio::test]
async fn adding_from_a_google_maps_link_takes_its_fields_from_the_place_details() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(PLACE_URL);
    transport.push_ok_get(200, &fixture("google_find_place.json"));
    transport.push_ok_get(200, PLACE_DETAILS);
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let location_id = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "The school",
            place_type_id: school_id,
            note: Some("the kids' school"),
            source_url: None,
            placement: Placement::GoogleMapsLink(SHORT_LINK),
        },
    )
    .await
    .unwrap();

    let row = location_row(&pool, location_id).await;
    let (_, place_type_id, member, name, lat, lon, state, note, source_url, external_id, payload) =
        row;
    assert_eq!(place_type_id, Some(school_id));
    assert_eq!(member, member_id);
    // The details' display name and position win over the caller's, the
    // position rounded to the six decimals the Rails app stored.
    assert_eq!(name, "Berlin Cosmopolitan School");
    assert_eq!((lat, lon), (52.526763, 13.407897));
    assert_eq!(state, "inbox");
    assert_eq!(note.as_deref(), Some("the kids' school"));
    assert_eq!(source_url.as_deref(), Some(SHORT_LINK));
    assert_eq!(
        external_id.as_deref(),
        Some("google_maps:place_id:ChIJtestplaceid")
    );
    let stored: serde_json::Value = serde_json::from_str(payload.as_deref().unwrap()).unwrap();
    let mut expected: serde_json::Value = serde_json::from_str(PLACE_DETAILS).unwrap();
    expected["version"] = serde_json::json!(2);
    assert_eq!(
        stored, expected,
        "the payload is stored whole, stamped version 2"
    );

    assert_eq!(transport.recorded_heads(), [SHORT_LINK]);
    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 2);
    assert_eq!(
        gets[0].url,
        "https://maps.googleapis.com/maps/api/place/findplacefromtext/json"
    );
    assert_eq!(
        gets[1].url,
        "https://places.googleapis.com/v1/places/ChIJtestplaceid"
    );
    assert_eq!(isochrones_job_count(&pool, location_id).await, 1);
}

#[tokio::test]
async fn adding_from_a_google_maps_link_keeps_the_given_name_when_the_details_carry_none() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(PLACE_URL);
    transport.push_ok_get(200, &fixture("google_find_place.json"));
    transport.push_ok_get(200, PLACE_DETAILS_WITHOUT_A_NAME);
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let location_id = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "The school",
            place_type_id: school_id,
            note: None,
            source_url: None,
            placement: Placement::GoogleMapsLink(SHORT_LINK),
        },
    )
    .await
    .unwrap();

    let (_, _, _, name, lat, lon, ..) = location_row(&pool, location_id).await;
    assert_eq!(name, "The school");
    assert_eq!((lat, lon), (52.526763, 13.407897));
}

#[tokio::test]
async fn adding_from_a_google_maps_link_is_refused_when_the_details_carry_no_position() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(PLACE_URL);
    transport.push_ok_get(200, &fixture("google_find_place.json"));
    transport.push_ok_get(200, PLACE_DETAILS_WITHOUT_A_POSITION);
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        NewLocation {
            name: "The school",
            place_type_id: school_id,
            note: None,
            source_url: None,
            placement: Placement::GoogleMapsLink(SHORT_LINK),
        },
    )
    .await
    .expect_err("details without a position cannot place a location");

    assert!(
        matches!(error, LocationError::IncompleteDetails),
        "expected an incomplete-details error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "the Google place details carry no position"
    );
    assert_eq!(location_count(&pool, relocation_id).await, 0);
}

#[tokio::test]
async fn adding_refuses_a_source_url_the_relocation_already_tracks() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let new_location = |name: &'static str| NewLocation {
        name,
        place_type_id: school_id,
        note: None,
        source_url: Some("https://example.com/listing/1"),
        placement: Placement::Coordinates(Coordinates {
            lat: 52.52,
            lng: 13.405,
        }),
    };
    add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        new_location("First"),
    )
    .await
    .unwrap();

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        new_location("Second"),
    )
    .await
    .expect_err("the source url is already tracked in this relocation");
    assert!(
        matches!(error, LocationError::DuplicateSourceUrl(ref url) if url == "https://example.com/listing/1"),
        "expected a duplicate-source-url error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "another location in this relocation already has the source url https://example.com/listing/1"
    );
    assert_eq!(location_count(&pool, relocation_id).await, 1);

    // The same url under another relocation is another location: the
    // uniqueness is scoped.
    let account_id: i64 = sqlx::query_scalar("SELECT account_id FROM relocations WHERE id = ?")
        .bind(relocation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let vienna_id = relocations::create(&pool, account_id, "Vienna", 48.208, 16.373, 11.0)
        .await
        .unwrap();
    let vienna_school = place_type_id(&pool, vienna_id, "School").await;
    add_location(
        &pool,
        &transport,
        vienna_id,
        member_id,
        NewLocation {
            name: "Vienna flat",
            place_type_id: vienna_school,
            note: None,
            source_url: Some("https://example.com/listing/1"),
            placement: Placement::Coordinates(Coordinates {
                lat: 48.208,
                lng: 16.373,
            }),
        },
    )
    .await
    .unwrap();
    assert_eq!(location_count(&pool, relocation_id).await, 1);
    assert_eq!(location_count(&pool, vienna_id).await, 1);
}

#[tokio::test]
async fn adding_refuses_an_external_identifier_the_relocation_already_tracks() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    // The first link resolves through the place url with its centre.
    transport.push_redirect_head(PLACE_URL);
    transport.push_ok_get(200, &fixture("google_find_place.json"));
    transport.push_ok_get(200, PLACE_DETAILS);
    // The second link resolves through one without a centre: a different
    // url, the same place.
    transport.push_redirect_head(PLACE_URL_WITHOUT_A_CENTRE);
    transport.push_ok_get(200, &fixture("google_find_place.json"));
    transport.push_ok_get(200, PLACE_DETAILS);
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    let from_link = |link: &'static str| NewLocation {
        name: "The school",
        place_type_id: school_id,
        note: None,
        source_url: None,
        placement: Placement::GoogleMapsLink(link),
    };
    add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        from_link(SHORT_LINK),
    )
    .await
    .unwrap();

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        from_link(SHORT_LINK_WITHOUT_A_CENTRE),
    )
    .await
    .expect_err("the place is already tracked in this relocation");
    assert!(
        matches!(
            error,
            LocationError::DuplicateExternalIdentifier(ref identifier)
                if identifier == "google_maps:place_id:ChIJtestplaceid"
        ),
        "expected a duplicate-external-identifier error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "another location in this relocation already has the external identifier \
         google_maps:place_id:ChIJtestplaceid"
    );
    assert_eq!(location_count(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn adding_refuses_a_place_type_or_member_from_outside_the_relocation() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let member_id = first_member_id(&pool).await;
    let school_id = place_type_id(&pool, relocation_id, "School").await;

    // A second relocation in the same account, and a member of another
    // account: neither's inputs belong to this relocation.
    let account_id: i64 = sqlx::query_scalar("SELECT account_id FROM relocations WHERE id = ?")
        .bind(relocation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let vienna_id = relocations::create(&pool, account_id, "Vienna", 48.208, 16.373, 11.0)
        .await
        .unwrap();
    let vienna_school = place_type_id(&pool, vienna_id, "School").await;
    let other_account = accounts::create_with_first_member(&pool, "stranger@example.com")
        .await
        .unwrap();

    let at_coordinates = |name: &'static str, place_type_id: i64| NewLocation {
        name,
        place_type_id,
        note: None,
        source_url: None,
        placement: Placement::Coordinates(Coordinates {
            lat: 52.52,
            lng: 13.405,
        }),
    };

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        member_id,
        at_coordinates("Vienna school", vienna_school),
    )
    .await
    .expect_err("a place type of another relocation cannot label this one's locations");
    assert!(
        matches!(
            error,
            LocationError::PlaceTypeNotFound { place_type_id, relocation_id: r }
                if place_type_id == vienna_school && r == relocation_id
        ),
        "expected a place-type refusal, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        format!("no place type {vienna_school} exists in relocation {relocation_id}")
    );

    let error = add_location(
        &pool,
        &transport,
        relocation_id,
        other_account.member_id,
        at_coordinates("Stranger's place", school_id),
    )
    .await
    .expect_err("a member of another account cannot write to this relocation");
    assert!(
        matches!(error, LocationError::MemberNotInAccount { member_id } if member_id == other_account.member_id),
        "expected a member refusal, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "member {} does not belong to the relocation's account",
            other_account.member_id
        )
    );

    assert_eq!(location_count(&pool, relocation_id).await, 0);
}

#[tokio::test]
async fn adding_refuses_an_unknown_relocation() {
    let (_directory, pool, _relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    let member_id = first_member_id(&pool).await;

    let error = add_location(
        &pool,
        &transport,
        999,
        member_id,
        NewLocation {
            name: "Nowhere",
            place_type_id: 1,
            note: None,
            source_url: None,
            placement: Placement::Coordinates(Coordinates {
                lat: 52.52,
                lng: 13.405,
            }),
        },
    )
    .await
    .expect_err("no relocation has id 999");

    assert!(
        matches!(error, LocationError::RelocationNotFound),
        "expected a relocation-not-found refusal, got: {error}"
    );
}
