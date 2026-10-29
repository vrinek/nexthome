mod common;

use common::{FakeTransport, fixture, relocated_pool};
use nexthome::{
    geo::Coordinates,
    google_maps::{GoogleMapsClient, GoogleMapsError, TravelMode},
};
use sqlx::SqlitePool;

const SHORT_LINK: &str = "https://maps.app.goo.gl/Rk5HAHR3rdiQgvJeA";
const FIND_PLACE_URL: &str = "https://maps.googleapis.com/maps/api/place/findplacefromtext/json";
const NEARBY_SEARCH_URL: &str = "https://maps.googleapis.com/maps/api/place/nearbysearch/json";

/// A client on the fake transport with a test key, as production would build
/// one on `ReqwestTransport` with the real key.
fn client<'a>(
    transport: &'a FakeTransport,
    pool: &'a SqlitePool,
    relocation_id: i64,
) -> GoogleMapsClient<'a> {
    GoogleMapsClient {
        transport,
        pool,
        relocation_id,
        api_key: "test-key",
    }
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

#[tokio::test]
async fn find_by_type_sends_the_search_as_query_parameters() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_nearby_page1.json"));
    // The token in the fixture would send the search after another page, so
    // the second page answers without one.
    transport.push_ok_get(200, &fixture("google_nearby_page2.json"));

    let results = client(&transport, &pool, relocation_id)
        .find_by_type(
            Some("school"),
            Coordinates {
                lat: 52.5,
                lng: 13.4,
            },
            None,
        )
        .await
        .unwrap();

    let place_ids: Vec<&str> = results
        .iter()
        .map(|place| place["place_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        place_ids,
        [
            "ChIJgTwKg7COqEcRZ0m2XW",
            "ChIJfXk8G1bOqEcRbtPf3S5",
            "ChIJhXz8M9WqEcRz8Qm2bT7"
        ],
        "both pages' places come back, in page order"
    );

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 2);
    assert_eq!(gets[0].url, NEARBY_SEARCH_URL);
    assert_eq!(
        gets[0].params(),
        vec![
            ("radius", "50000"),
            ("location", "52.5,13.4"),
            ("types", "school"),
            ("key", "test-key"),
        ]
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 2);
}

#[tokio::test]
async fn find_by_type_sends_the_keyword_without_an_empty_type() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(
        200,
        r#"{"html_attributions": [], "results": [], "status": "OK"}"#,
    );

    let results = client(&transport, &pool, relocation_id)
        .find_by_type(
            None,
            Coordinates {
                lat: 52.5,
                lng: 13.4,
            },
            Some("gym"),
        )
        .await
        .unwrap();

    assert!(results.is_empty());

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].params(),
        vec![
            ("radius", "50000"),
            ("location", "52.5,13.4"),
            ("keyword", "gym"),
            ("key", "test-key"),
        ],
        "an absent type must not reach the wire"
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn find_by_type_follows_the_pagination_token_and_retries_a_not_yet_valid_one() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_nearby_page1.json"));
    // Google answers INVALID_REQUEST when the follow-up comes before the
    // token is valid; the wait and retry must then land the second page.
    transport.push_ok_get(
        200,
        r#"{"html_attributions": [], "results": [], "status": "INVALID_REQUEST"}"#,
    );
    transport.push_ok_get(200, &fixture("google_nearby_page2.json"));

    let results = client(&transport, &pool, relocation_id)
        .find_by_type(
            Some("school"),
            Coordinates {
                lat: 52.5,
                lng: 13.4,
            },
            None,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 3);

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 3);
    // Every follow-up page carries only the token and the key, as the Rails
    // app's spec pinned down.
    for follow_up in &gets[1..] {
        assert_eq!(follow_up.url, NEARBY_SEARCH_URL);
        assert_eq!(
            follow_up.params(),
            vec![
                (
                    "pagetoken",
                    "CrYCKhIA5pJjTkw2gvkUQa4oLbeG0bVNQtFzTQh5bZeGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBpGk2fXrBq4oL"
                ),
                ("key", "test-key"),
            ]
        );
    }
    assert_eq!(google_calls(&pool, relocation_id).await, 3);
}

#[tokio::test]
async fn find_by_type_errors_when_google_denies_the_search() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_nearby_denied.json"));

    let error = client(&transport, &pool, relocation_id)
        .find_by_type(
            Some("school"),
            Coordinates {
                lat: 52.5,
                lng: 13.4,
            },
            None,
        )
        .await
        .expect_err("a denied search must not look like an empty one");

    assert!(
        matches!(error, GoogleMapsError::ApiStatus { .. }),
        "expected an api-status error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "Google nearby search answered REQUEST_DENIED: The provided API key is invalid."
    );
    // The request was made and counted, even though Google refused it.
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn find_by_type_gives_up_when_the_page_token_never_becomes_valid() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_nearby_page1.json"));
    for _ in 0..6 {
        transport.push_ok_get(
            200,
            r#"{"html_attributions": [], "results": [], "status": "INVALID_REQUEST"}"#,
        );
    }

    let error = client(&transport, &pool, relocation_id)
        .find_by_type(
            Some("school"),
            Coordinates {
                lat: 52.5,
                lng: 13.4,
            },
            None,
        )
        .await
        .expect_err("a token that stays invalid must end the search, not loop forever");

    assert!(
        matches!(error, GoogleMapsError::InvalidPageToken(_)),
        "expected an invalid-page-token error, got: {error}"
    );
    // The first page, then the follow-up and its five retries.
    assert_eq!(transport.recorded_gets().len(), 7);
    assert_eq!(google_calls(&pool, relocation_id).await, 7);
}

#[tokio::test]
async fn place_id_from_shared_link_biases_the_search_to_the_map_centre() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(
        "https://www.google.com/maps/place/Berlin+Cosmopolitan+School\
         /@52.5267634,13.407897,16.17z/data=!4m6!3m5!1s0x47a8:0x3b12!16s%2Fg%2F09gd1sv",
    );
    transport.push_ok_get(200, &fixture("google_find_place.json"));

    let place_id = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .unwrap();

    assert_eq!(place_id, "ChIJtestplaceid");

    assert_eq!(transport.recorded_heads(), [SHORT_LINK]);
    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].url, FIND_PLACE_URL);
    // The url's zoom must be dropped: a bias carrying it makes Google reject
    // the request.
    assert_eq!(
        gets[0].params(),
        vec![
            ("input", "Berlin Cosmopolitan School"),
            ("fields", "place_id"),
            ("inputtype", "textquery"),
            ("locationbias", "circle:10@52.5267634,13.407897"),
            ("key", "test-key"),
        ]
    );
    // Expanding the short link is a redirect on Google's link service, not a
    // billable API request: only the search counts.
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn place_id_from_shared_link_searches_without_a_bias_when_the_url_has_no_centre() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(
        "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/data=!4m2",
    );
    transport.push_ok_get(200, &fixture("google_find_place.json"));

    let place_id = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .unwrap();

    assert_eq!(place_id, "ChIJtestplaceid");

    let gets = transport.recorded_gets();
    assert_eq!(
        gets[0].params(),
        vec![
            ("input", "Berlin Cosmopolitan School"),
            ("fields", "place_id"),
            ("inputtype", "textquery"),
            ("key", "test-key"),
        ],
        "no map centre in the url, so no locationbias"
    );
}

#[tokio::test]
async fn place_id_from_shared_link_errors_when_google_denies_the_search() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(
        "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/@52.5267634,13.407897,16.17z",
    );
    transport.push_ok_get(
        200,
        r#"{"candidates": [], "error_message": "You have exceeded your request quota for this API.", "status": "REQUEST_DENIED"}"#,
    );

    let error = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .expect_err("a denied search must not look like an unmatched name");

    assert!(
        matches!(error, GoogleMapsError::ApiStatus { .. }),
        "expected an api-status error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "Google findplacefromtext answered REQUEST_DENIED: You have exceeded your request quota for this API."
    );
}

#[tokio::test]
async fn place_id_from_shared_link_errors_when_google_finds_no_place() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(
        "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/@52.5267634,13.407897,16.17z",
    );
    transport.push_ok_get(200, r#"{"candidates": [], "status": "ZERO_RESULTS"}"#);

    let error = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .expect_err("a single-place lookup has no legitimate empty answer");

    assert!(
        matches!(error, GoogleMapsError::ApiStatus { .. }),
        "expected an api-status error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "Google findplacefromtext answered ZERO_RESULTS"
    );
}

#[tokio::test]
async fn place_id_from_shared_link_reads_the_place_name_out_of_a_q_url() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head("https://maps.google.com/?q=Berlin+Cosmopolitan+School&hl=en");
    transport.push_ok_get(200, &fixture("google_find_place.json"));

    let place_id = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .unwrap();

    assert_eq!(place_id, "ChIJtestplaceid");

    let gets = transport.recorded_gets();
    assert_eq!(gets[0].params()[0], ("input", "Berlin Cosmopolitan School"));
}

#[tokio::test]
async fn place_id_from_shared_link_refuses_to_guess_when_google_returns_more_than_one_candidate() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head(
        "https://www.google.com/maps/place/Berlin+Cosmopolitan+School/@52.5267634,13.407897,16.17z",
    );
    transport.push_ok_get(
        200,
        r#"{"candidates": [{"place_id": "a"}, {"place_id": "b"}], "status": "OK"}"#,
    );

    let error = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .expect_err("two candidates leave the place ambiguous, so the link must not resolve");

    assert!(
        matches!(error, GoogleMapsError::AmbiguousPlace { candidates: 2, .. }),
        "expected an ambiguous-place error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "too many results for Berlin Cosmopolitan School: 2"
    );
}

#[tokio::test]
async fn place_id_from_shared_link_reports_a_url_it_does_not_recognise() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_redirect_head("https://example.com/foo");

    let error = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .expect_err("an expanded url in no known form must be reported");

    assert!(
        matches!(error, GoogleMapsError::UnrecognisedLink(_)),
        "expected an unrecognised-link error, got: {error}"
    );
    assert!(
        error.to_string().contains("https://example.com/foo"),
        "the message must show the url: {error}"
    );
    // Nothing reached the API, so nothing counts.
    assert!(transport.recorded_gets().is_empty());
    assert_eq!(google_calls(&pool, relocation_id).await, 0);
}

#[tokio::test]
async fn place_id_from_shared_link_refuses_a_link_that_does_not_redirect() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_head(200);

    let error = client(&transport, &pool, relocation_id)
        .place_id_from_shared_link(SHORT_LINK)
        .await
        .expect_err("a HEAD that does not answer 302 leaves nothing to expand");

    assert!(
        matches!(error, GoogleMapsError::CantExpand(_)),
        "expected a can't-expand error, got: {error}"
    );
    assert!(
        error.to_string().contains(SHORT_LINK),
        "the message must show the link: {error}"
    );
    assert!(transport.recorded_gets().is_empty());
}

#[tokio::test]
async fn details_by_place_id_returns_the_places_api_v1_details() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_place_details.json"));

    let details = client(&transport, &pool, relocation_id)
        .details_by_place_id("ChIJN1t_tDeuEmsRUsoyG83frY4")
        .await
        .unwrap();

    let expected: serde_json::Value =
        serde_json::from_str(&fixture("google_place_details.json")).unwrap();
    assert_eq!(details, expected);

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://places.googleapis.com/v1/places/ChIJN1t_tDeuEmsRUsoyG83frY4"
    );
    assert_eq!(
        gets[0].params(),
        vec![
            (
                "fields",
                "businessStatus,location,id,plusCode,types,googleMapsUri,websiteUri,displayName"
            ),
            ("key", "test-key"),
        ]
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn details_by_place_id_fails_on_a_non_200_answer() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(404, "Place not found");

    let error = client(&transport, &pool, relocation_id)
        .details_by_place_id("ChIJmissing")
        .await
        .expect_err("a non-200 answer carries no details");

    assert!(
        matches!(error, GoogleMapsError::DetailsFailed(404)),
        "expected a details-failed error with the status, got: {error}"
    );
}

#[tokio::test]
async fn coordinates_from_address_returns_the_first_results_geometry() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_geocode.json"));

    let coordinates = client(&transport, &pool, relocation_id)
        .coordinates_from_address("Inselstraße 10, Berlin")
        .await
        .unwrap();

    assert_eq!(
        coordinates,
        Coordinates {
            lat: 52.5106,
            lng: 13.4106
        }
    );

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://maps.googleapis.com/maps/api/geocode/json"
    );
    assert_eq!(
        gets[0].params(),
        vec![("address", "Inselstraße 10, Berlin"), ("key", "test-key")]
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn coordinates_from_address_fails_on_a_non_200_answer() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(500, "internal error");

    let error = client(&transport, &pool, relocation_id)
        .coordinates_from_address("Inselstraße 10, Berlin")
        .await
        .expect_err("a non-200 answer carries no geometry");

    assert!(
        matches!(error, GoogleMapsError::GeocodeFailed(500)),
        "expected a geocode-failed error with the status, got: {error}"
    );
}

#[tokio::test]
async fn coordinates_from_address_errors_when_google_denies_the_geocode() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(
        200,
        r#"{"results": [], "error_message": "You have exceeded your request quota for this API.", "status": "REQUEST_DENIED"}"#,
    );

    let error = client(&transport, &pool, relocation_id)
        .coordinates_from_address("Inselstraße 10, Berlin")
        .await
        .expect_err("a denied geocode must not look like an unfound address");

    assert!(
        matches!(error, GoogleMapsError::ApiStatus { .. }),
        "expected an api-status error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "Google geocode answered REQUEST_DENIED: You have exceeded your request quota for this API."
    );
}

#[tokio::test]
async fn coordinates_from_address_fails_when_geocoding_answers_no_result() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, r#"{"results": [], "status": "ZERO_RESULTS"}"#);

    let error = client(&transport, &pool, relocation_id)
        .coordinates_from_address("No Such Street 1, Berlin")
        .await
        .expect_err("an empty result list leaves no geometry to take");

    assert!(
        matches!(error, GoogleMapsError::NoGeocodeResults(_)),
        "expected a no-results error, got: {error}"
    );
    assert!(
        error.to_string().contains("No Such Street 1, Berlin"),
        "the message must name the address: {error}"
    );
}

#[tokio::test]
async fn route_matrix_returns_travel_minutes_for_every_pair() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(200, &fixture("google_distance_matrix.json"));

    let minutes = client(&transport, &pool, relocation_id)
        .route_matrix(
            &[Coordinates {
                lat: 52.52,
                lng: 13.405,
            }],
            &[
                Coordinates {
                    lat: 52.5122,
                    lng: 13.4512,
                },
                Coordinates {
                    lat: 52.5219,
                    lng: 13.4139,
                },
            ],
            TravelMode::Bicycling,
        )
        .await
        .unwrap();

    // One row per origin, one entry per destination, in the order sent;
    // an unreachable pair is None.
    assert_eq!(minutes, vec![vec![Some(22), None]]);

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(
        gets[0].url,
        "https://maps.googleapis.com/maps/api/distancematrix/json"
    );
    assert_eq!(
        gets[0].params(),
        vec![
            ("origins", "52.52,13.405"),
            ("destinations", "52.5122,13.4512|52.5219,13.4139"),
            ("mode", "bicycling"),
            ("key", "test-key"),
        ]
    );
    assert_eq!(google_calls(&pool, relocation_id).await, 1);
}

#[tokio::test]
async fn route_matrix_fails_when_google_answers_a_non_ok_status() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    transport.push_ok_get(
        200,
        r#"{"destination_addresses": [], "origin_addresses": [], "rows": [], "status": "OVER_QUERY_LIMIT"}"#,
    );

    let error = client(&transport, &pool, relocation_id)
        .route_matrix(
            &[Coordinates {
                lat: 52.52,
                lng: 13.405,
            }],
            &[Coordinates {
                lat: 52.5122,
                lng: 13.4512,
            }],
            TravelMode::Transit,
        )
        .await
        .expect_err("a non-OK status carries no usable durations");

    assert!(
        matches!(error, GoogleMapsError::MatrixStatus(ref status) if status == "OVER_QUERY_LIMIT"),
        "expected a matrix-status error with the status, got: {error}"
    );
}

#[tokio::test]
async fn route_matrix_fails_when_the_answer_does_not_match_the_requested_shape() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let transport = FakeTransport::new();
    // Two rows for one origin: the entries could not be matched to pairs.
    transport.push_ok_get(
        200,
        r#"{
            "destination_addresses": ["Neumark 12, 10117 Berlin"],
            "origin_addresses": ["Inselstraße 10, 10179 Berlin", "Turmstraße 1, 10559 Berlin"],
            "rows": [
                {"elements": [{"status": "OK", "duration": {"text": "10 mins", "value": 600}}]},
                {"elements": [{"status": "OK", "duration": {"text": "25 mins", "value": 1500}}]}
            ],
            "status": "OK"
        }"#,
    );

    let error = client(&transport, &pool, relocation_id)
        .route_matrix(
            &[Coordinates {
                lat: 52.52,
                lng: 13.405,
            }],
            &[Coordinates {
                lat: 52.5122,
                lng: 13.4512,
            }],
            TravelMode::Driving,
        )
        .await
        .expect_err("an answer that does not line up with the request must be refused");

    assert!(
        matches!(error, GoogleMapsError::MatrixShape(_)),
        "expected a matrix-shape error, got: {error}"
    );
    assert_eq!(
        error.to_string(),
        "route matrix answer does not match: 2 rows for 1 origins"
    );
}
