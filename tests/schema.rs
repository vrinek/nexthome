use std::str::FromStr;

use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

#[tokio::test]
async fn migrations_apply_and_enforce_constraints() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    // SQLite enforces foreign keys only when the connection opts in, so the
    // orphan-row assertion below depends on this option.
    let options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();

    sqlx::raw_sql(
        r#"
        INSERT INTO accounts (id, created_at) VALUES (1, '2026-10-24T08:00:00Z');
        INSERT INTO members (id, account_id, email, api_key_hash, created_at)
            VALUES (1, 1, 'mover@example.com', 'a1b2c3', '2026-10-24T08:00:00Z');
        INSERT INTO relocations (id, account_id, city_name, lat, lon, zoom, paid_at, expires_at, archived_at, stripe_checkout_session_id)
            VALUES (1, 1, 'Berlin', 52.52, 13.405, 11.5, '2026-10-24T09:00:00Z', '2027-04-24T09:00:00Z', NULL, 'cs_test_1');
        INSERT INTO place_types (id, relocation_id, name, icon, color, search_type, search_keyword)
            VALUES (1, 1, 'School', 'school', '#1a73e8', 'school', 'Grundschule');
        INSERT INTO locations (id, relocation_id, place_type_id, member_id, name, lat, lon, state, note, source_url, external_identifier, google_maps_payload, created_at, updated_at)
            VALUES (1, 1, 1, 1, 'Grundschule am Neumark', 52.51, 13.4, 'accepted', 'near the park', 'https://example.com/places/1', 'ChIJgFK', '{}', '2026-10-24T10:00:00Z', '2026-10-24T10:00:00Z');
        INSERT INTO isochrones (id, location_id, minutes, geojson)
            VALUES (1, 1, 20, '{"type": "Polygon", "coordinates": []}');
        INSERT INTO evaluations (id, location_id, computed_at, rows)
            VALUES (1, 1, '2026-10-24T11:00:00Z', '[{"place_type": "School"}]');
        INSERT INTO jobs (id, kind, payload, run_after, attempts, last_error, done_at)
            VALUES (1, 'isochrones', '{"location_id": 1}', '2026-10-24T11:00:00Z', 0, NULL, NULL);
        INSERT INTO api_usage (id, relocation_id, provider, month, calls)
            VALUES (1, 1, 'mapbox', 202610, 3);
        INSERT INTO issues (id, member_id, body, created_at)
            VALUES (1, 1, 'heatmap does not load', '2026-10-24T12:00:00Z');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let orphan = sqlx::query("INSERT INTO issues (member_id, body) VALUES (999, 'orphan')")
        .execute(&pool)
        .await
        .unwrap_err();
    assert!(
        orphan.to_string().contains("FOREIGN KEY constraint failed"),
        "member 999 does not exist, expected a foreign key violation: {orphan}"
    );

    let duplicate = sqlx::query(
        "INSERT INTO locations (id, relocation_id, place_type_id, member_id, name, lat, lon, state, source_url)
         VALUES (2, 1, 1, 1, 'same place twice', 52.52, 13.41, 'inbox', 'https://example.com/places/1')",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        duplicate
            .to_string()
            .contains("UNIQUE constraint failed: locations.relocation_id, locations.source_url"),
        "the same source_url twice in one relocation, expected a unique violation: {duplicate}"
    );

    pool.close().await;
}
