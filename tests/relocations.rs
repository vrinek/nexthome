use std::str::FromStr;

use nexthome::{
    accounts,
    relocations::{self, PlaceType, RelocationError},
};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

/// One place_types row as the tests read it back: name, icon, color,
/// search_type, search_keyword.
type PlaceTypeRow = (String, String, String, Option<String>, Option<String>);

async fn migrated_pool() -> (tempfile::TempDir, SqlitePool) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("nexthome.db").display()
    );
    // SQLite enforces foreign keys only when the connection opts in, and
    // main.rs runs with them on, so these tests do too.
    let options = SqliteConnectOptions::from_str(&url)
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    (directory, pool)
}

#[tokio::test]
async fn creating_a_relocation_seeds_its_place_types() {
    let (_directory, pool) = migrated_pool().await;

    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let relocation_id =
        relocations::create(&pool, created.account_id, "Berlin", 52.52, 13.405, 11.5)
            .await
            .unwrap();

    let (city_name, lat, lon, zoom): (String, f64, f64, f64) =
        sqlx::query_as("SELECT city_name, lat, lon, zoom FROM relocations WHERE id = ?")
            .bind(relocation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(city_name, "Berlin");
    assert_eq!((lat, lon, zoom), (52.52, 13.405, 11.5));

    // A relocation starts unpaid and unarchived; activation is a separate
    // step that records payment.
    let (paid_at, expires_at, archived_at): (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT paid_at, expires_at, archived_at FROM relocations WHERE id = ?")
            .bind(relocation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(paid_at.is_none());
    assert!(expires_at.is_none());
    assert!(archived_at.is_none());

    // The seed list lands as one row per entry, in order.
    let seeded: Vec<PlaceTypeRow> = sqlx::query_as(
        "SELECT name, icon, color, search_type, search_keyword
         FROM place_types WHERE relocation_id = ? ORDER BY id",
    )
    .bind(relocation_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        seeded,
        vec![
            (
                "School".into(),
                "school".into(),
                "1982c4".into(),
                Some("school".into()),
                None
            ),
            (
                "Gym".into(),
                "basketball".into(),
                "ff595e".into(),
                None,
                Some("gym".into())
            ),
            (
                "Apartment".into(),
                "lodging".into(),
                "2a9d8f".into(),
                Some("apartment".into()),
                None
            ),
            (
                "Martial arts school".into(),
                "fitness-centre".into(),
                "8338ec".into(),
                None,
                Some("martial arts".into())
            ),
            (
                "School bus stop".into(),
                "bus".into(),
                "c78d54".into(),
                None,
                None
            ),
        ]
    );

    pool.close().await;
}

#[tokio::test]
async fn add_place_type_refuses_an_archived_relocation_and_accepts_an_unarchived_one() {
    let (_directory, pool) = migrated_pool().await;

    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let unarchived_id =
        relocations::create(&pool, created.account_id, "Berlin", 52.52, 13.405, 11.5)
            .await
            .unwrap();
    let archived_id =
        relocations::create(&pool, created.account_id, "Vienna", 48.208, 16.373, 11.0)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE relocations SET archived_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?",
    )
    .bind(archived_id)
    .execute(&pool)
    .await
    .unwrap();

    let pharmacy = PlaceType {
        name: "Pharmacy",
        icon: "local-pharmacy",
        color: "4361ee",
        search_type: Some("pharmacy"),
        search_keyword: None,
    };

    let place_type_id = relocations::add_place_type(&pool, unarchived_id, pharmacy)
        .await
        .unwrap();
    let stored: PlaceTypeRow = sqlx::query_as(
        "SELECT name, icon, color, search_type, search_keyword FROM place_types WHERE id = ?",
    )
    .bind(place_type_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        stored,
        (
            "Pharmacy".into(),
            "local-pharmacy".into(),
            "4361ee".into(),
            Some("pharmacy".into()),
            None
        )
    );

    let error = relocations::add_place_type(&pool, archived_id, pharmacy)
        .await
        .expect_err("an archived relocation is read-only");
    assert!(
        matches!(error, RelocationError::Archived),
        "expected an archive refusal, got: {error}"
    );
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM place_types WHERE relocation_id = ? AND name = 'Pharmacy'",
    )
    .bind(archived_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(refused, 0, "the refused write must store nothing");

    let error = relocations::add_place_type(&pool, 999, pharmacy)
        .await
        .expect_err("no relocation has id 999");
    assert!(
        matches!(error, RelocationError::NotFound),
        "expected a not-found refusal, got: {error}"
    );

    pool.close().await;
}

#[tokio::test]
async fn activating_sets_expires_at_exactly_six_calendar_months_after_paid_at() {
    let (_directory, pool) = migrated_pool().await;

    let created = accounts::create_with_first_member(&pool, "mover@example.com")
        .await
        .unwrap();
    let relocation_id =
        relocations::create(&pool, created.account_id, "Berlin", 52.52, 13.405, 11.5)
            .await
            .unwrap();

    let activated = relocations::activate(&pool, relocation_id).await.unwrap();

    // paid_at comes from SQLite's strftime; the shape check catches a
    // format-string typo.
    assert_eq!(activated.paid_at.len(), 20);
    assert!(activated.paid_at.ends_with('Z'));

    // expires_at must be exactly paid_at advanced by six calendar months
    // (SQLite's '+6 months' modifier), and the returned values must be the
    // stored ones.
    let (paid_at, expires_at, six_months_on): (String, String, String) = sqlx::query_as(
        "SELECT paid_at, expires_at, strftime('%Y-%m-%dT%H:%M:%SZ', paid_at, '+6 months')
         FROM relocations WHERE id = ?",
    )
    .bind(relocation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(activated.paid_at, paid_at);
    assert_eq!(activated.expires_at, expires_at);
    assert_eq!(
        expires_at, six_months_on,
        "expires_at must be exactly six calendar months after paid_at"
    );

    pool.close().await;
}
