use std::str::FromStr;

use nexthome::{accounts, relocations};
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
