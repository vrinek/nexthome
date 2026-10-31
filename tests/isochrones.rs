//! Isochrones over a real database: generating a location's walking
//! contours from Mapbox, skipping the ones it already has, and the solo
//! and intersection heatmaps the contours combine into — reproduced on
//! fixture polygons. No test calls a live provider.

mod common;

use std::sync::Arc;

use common::{FakeTransport, fixture, mapbox_access, relocated_pool};
use nexthome::isochrones::{self, Contour};
use serde_json::{Value, json};
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

/// The school contours around the origin: half-widths 1, 2, 3, 4 and 6
/// at 5, 10, 15, 20 and 30 minutes — nested squares, so every pair's
/// intersection is the smaller square.
fn school_contours() -> Vec<Contour> {
    [(5, 1.0), (10, 2.0), (15, 3.0), (20, 4.0), (30, 6.0)]
        .into_iter()
        .map(|(minutes, half_width)| square_contour(minutes, (0.0, 0.0), half_width))
        .collect()
}

/// The gym contours around the origin: half-widths 1, 5 and 2 at the 5,
/// 10 and 15 minutes the intersection reads, and 7 and 8 at the 20 and
/// 30 minutes it never reads — reading them would show in the red
/// layer's area.
fn gym_contours() -> Vec<Contour> {
    [(5, 1.0), (10, 5.0), (15, 2.0), (20, 7.0), (30, 8.0)]
        .into_iter()
        .map(|(minutes, half_width)| square_contour(minutes, (0.0, 0.0), half_width))
        .collect()
}

/// One square contour: a Mapbox-shaped FeatureCollection holding the
/// axis-aligned square of the given half-width around the center.
fn square_contour(minutes: i64, center: (f64, f64), half_width: f64) -> Contour {
    let (x, y) = center;
    let ring = [
        (x - half_width, y - half_width),
        (x + half_width, y - half_width),
        (x + half_width, y + half_width),
        (x - half_width, y + half_width),
        (x - half_width, y - half_width),
    ]
    .into_iter()
    .map(|(ring_x, ring_y)| json!([ring_x, ring_y]))
    .collect::<Vec<_>>();
    Contour {
        minutes,
        geojson: json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [ring]}
            }]
        }),
    }
}

/// The layers' names, in the order the heatmap lists them.
fn layer_names(layers: &[(String, Value)]) -> Vec<&str> {
    layers.iter().map(|(name, _)| name.as_str()).collect()
}

/// Asserts the layer covers exactly the expected area.
fn assert_layer_area(layers: &[(String, Value)], name: &str, expected: f64) {
    let (_, geojson) = layers
        .iter()
        .find(|(layer_name, _)| layer_name == name)
        .unwrap_or_else(|| panic!("no {name} layer"));
    let actual = area(geojson);
    assert!(
        (actual - expected).abs() < 1e-9,
        "the {name} layer covers {actual}, expected {expected}"
    );
}

/// The area a layer covers: the shoelace areas of its polygons, holes
/// subtracted — an independent check, not the `geo` crate's own.
fn area(geojson: &Value) -> f64 {
    geojson["features"]
        .as_array()
        .map(|features| features.iter().map(feature_area).sum())
        .unwrap_or(0.0)
}

fn feature_area(feature: &Value) -> f64 {
    let geometry = &feature["geometry"];
    match geometry["type"].as_str() {
        Some("Polygon") => polygon_area(&geometry["coordinates"]),
        Some("MultiPolygon") => geometry["coordinates"]
            .as_array()
            .map(|polygons| polygons.iter().map(polygon_area).sum())
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

fn polygon_area(coordinates: &Value) -> f64 {
    let Some(rings) = coordinates.as_array() else {
        return 0.0;
    };
    let Some((exterior, holes)) = rings.split_first() else {
        return 0.0;
    };
    ring_area(exterior).abs() - holes.iter().map(|hole| ring_area(hole).abs()).sum::<f64>()
}

fn ring_area(ring: &Value) -> f64 {
    let points = ring_points(ring);
    points
        .windows(2)
        .map(|pair| pair[0].0 * pair[1].1 - pair[1].0 * pair[0].1)
        .sum::<f64>()
        / 2.0
}

/// Whether the position sits inside the layer: an even-odd ray cast
/// over its rings, holes included — an independent check, not the `geo`
/// crate's own.
fn contains(geojson: &Value, x: f64, y: f64) -> bool {
    geojson["features"]
        .as_array()
        .map(|features| {
            features
                .iter()
                .any(|feature| feature_contains(feature, x, y))
        })
        .unwrap_or(false)
}

fn feature_contains(feature: &Value, x: f64, y: f64) -> bool {
    let geometry = &feature["geometry"];
    match geometry["type"].as_str() {
        Some("Polygon") => polygon_contains(&geometry["coordinates"], x, y),
        Some("MultiPolygon") => geometry["coordinates"]
            .as_array()
            .map(|polygons| {
                polygons
                    .iter()
                    .any(|polygon| polygon_contains(polygon, x, y))
            })
            .unwrap_or(false),
        _ => false,
    }
}

fn polygon_contains(coordinates: &Value, x: f64, y: f64) -> bool {
    let Some(rings) = coordinates.as_array() else {
        return false;
    };
    let Some((exterior, holes)) = rings.split_first() else {
        return false;
    };
    ring_contains(exterior, x, y) && holes.iter().all(|hole| !ring_contains(hole, x, y))
}

fn ring_contains(ring: &Value, x: f64, y: f64) -> bool {
    let points = ring_points(ring);
    let mut inside = false;
    for (index, &(point_x, point_y)) in points.iter().enumerate() {
        let (other_x, other_y) = points[(index + points.len() - 1) % points.len()];
        if (point_y > y) != (other_y > y)
            && x < (other_x - point_x) * (y - point_y) / (other_y - point_y) + point_x
        {
            inside = !inside;
        }
    }
    inside
}

fn ring_points(ring: &Value) -> Vec<(f64, f64)> {
    ring.as_array()
        .map(|points| {
            points
                .iter()
                .map(|point| match (point[0].as_f64(), point[1].as_f64()) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or(None)
        .unwrap_or_default()
}

/// Stores a contour row for the location, as the generation would have.
async fn store_contour(pool: &SqlitePool, location_id: i64, contour: &Contour) {
    sqlx::query("INSERT INTO isochrones (location_id, minutes, geojson) VALUES (?, ?, ?)")
        .bind(location_id)
        .bind(contour.minutes)
        .bind(contour.geojson.to_string())
        .execute(pool)
        .await
        .unwrap();
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

#[test]
fn solo_heatmap_bands_one_group_s_contours_without_overlap() {
    // Two locations with different half-widths, so a band built from the
    // wrong contour shows in its area; the 15-minute contours take part
    // in no solo band, as in the Rails heatmap.
    let near = (0.0, 0.0);
    let far = (100.0, 0.0);
    let group = [(5, 1.0), (10, 2.0), (15, 3.0), (20, 4.0), (30, 6.0)]
        .into_iter()
        .map(|(minutes, half_width)| square_contour(minutes, near, half_width))
        .chain(
            [(5, 0.5), (10, 1.5), (15, 2.5), (20, 3.5), (30, 5.5)]
                .into_iter()
                .map(|(minutes, half_width)| square_contour(minutes, far, half_width)),
        )
        .collect::<Vec<_>>();

    let layers = isochrones::solo_heatmap(&group);

    assert_eq!(
        layer_names(&layers),
        ["0-5", "6-10", "11-20", "21-30"],
        "the solo bands draw smallest minutes first"
    );
    // Near: 2², 4²−2², 8²−4², 12²−8². Far: 1², 3²−1², 7²−3², 11²−7².
    assert_layer_area(&layers, "0-5", 5.0);
    assert_layer_area(&layers, "6-10", 20.0);
    assert_layer_area(&layers, "11-20", 88.0);
    assert_layer_area(&layers, "21-30", 152.0);
    // Each position sits in exactly one band, around its own location.
    let positions = [
        (0.5, 0.5, "0-5"),
        (1.5, 0.5, "6-10"),
        (3.5, 0.5, "11-20"),
        (5.5, 0.5, "21-30"),
        (100.3, 0.1, "0-5"),
        (101.0, 0.0, "6-10"),
    ];
    for (x, y, band) in positions {
        for (name, geojson) in &layers {
            assert_eq!(
                contains(geojson, x, y),
                name == band,
                "({x}, {y}) must sit in the {band} band and no other, not {name}"
            );
        }
    }
}

#[test]
fn intersection_heatmap_bands_the_two_groups_shared_area_by_colour() {
    let layers = isochrones::intersection_heatmap(&school_contours(), &gym_contours());

    assert_eq!(
        layer_names(&layers),
        ["blue", "green", "yellow", "red"],
        "the colours draw best combination first"
    );
    // The nested squares make every pair's intersection the smaller
    // square: blue is min(1, ·), green min(3, ·) minus blue, yellow
    // min(4, ·) minus green, red min(6, 5) minus yellow.
    assert_layer_area(&layers, "blue", 4.0);
    assert_layer_area(&layers, "green", 32.0);
    assert_layer_area(&layers, "yellow", 28.0);
    assert_layer_area(&layers, "red", 36.0);
    // Walking outward from the centre, each distance lands in exactly
    // one colour.
    for (x, colour) in [(0.5, "blue"), (2.0, "green"), (3.5, "yellow"), (4.5, "red")] {
        for (name, geojson) in &layers {
            assert_eq!(
                contains(geojson, x, 0.0),
                name == colour,
                "({x}, 0) must sit in the {colour} layer and no other, not {name}"
            );
        }
    }
}

#[tokio::test]
async fn the_heatmap_combines_two_place_types_over_their_stored_contours() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let school_type = place_type_id(&pool, relocation_id, "School").await;
    let gym_type = place_type_id(&pool, relocation_id, "Gym").await;
    let school = common::insert_location(&pool, relocation_id, Some(school_type), 0.0, 0.0).await;
    let gym = common::insert_location(&pool, relocation_id, Some(gym_type), 0.0, 0.0).await;
    for contour in &school_contours() {
        store_contour(&pool, school, contour).await;
    }
    for contour in &gym_contours() {
        store_contour(&pool, gym, contour).await;
    }

    let heatmap = isochrones::heatmap(&pool, relocation_id, school_type, gym_type)
        .await
        .unwrap();

    assert_eq!(layer_names(&heatmap), ["blue", "green", "yellow", "red"]);
    assert_eq!(
        heatmap,
        isochrones::intersection_heatmap(&school_contours(), &gym_contours()),
        "the entry point combines exactly the two place types' contours"
    );
}

#[tokio::test]
async fn the_heatmap_is_empty_layers_when_either_side_has_no_isochrones() {
    let (_directory, pool, relocation_id) = relocated_pool().await;
    let school_type = place_type_id(&pool, relocation_id, "School").await;
    let gym_type = place_type_id(&pool, relocation_id, "Gym").await;
    // A school with contours; a gym location whose isochrones have not
    // been generated yet; and a place type the relocation does not
    // track.
    let school = common::insert_location(&pool, relocation_id, Some(school_type), 0.0, 0.0).await;
    for contour in &school_contours() {
        store_contour(&pool, school, contour).await;
    }
    common::insert_location(&pool, relocation_id, Some(gym_type), 0.0, 0.0).await;

    for (first, second) in [
        (school_type, gym_type),
        (gym_type, school_type),
        (school_type, 999),
    ] {
        let heatmap = isochrones::heatmap(&pool, relocation_id, first, second)
            .await
            .unwrap();
        assert_eq!(
            layer_names(&heatmap),
            ["blue", "green", "yellow", "red"],
            "a side without isochrones keeps the layer structure"
        );
        for (name, geojson) in &heatmap {
            assert_eq!(
                geojson["features"].as_array().map(Vec::len),
                Some(0),
                "the {name} layer carries nothing"
            );
        }
    }
}
