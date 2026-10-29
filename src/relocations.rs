use sqlx::{Executor, Sqlite, SqlitePool};

/// A place type's data: the fields of a `place_types` row before it is
/// inserted. The seed list below and `add_place_type` callers both supply
/// one.
#[derive(Clone, Copy)]
pub struct PlaceType<'a> {
    pub name: &'a str,
    pub icon: &'a str,
    pub color: &'a str,
    /// The Google Places type to search for when filling this place type
    /// with nearby places; None when the search runs on `search_keyword`.
    pub search_type: Option<&'a str>,
    /// The Google Places text to search for when `search_type` is None.
    /// Both None means places of this type are only ever added by hand.
    pub search_keyword: Option<&'a str>,
}

/// The place types every new relocation starts with, seeded on creation.
pub const SEED_PLACE_TYPES: [PlaceType<'static>; 5] = [
    PlaceType {
        name: "School",
        icon: "school",
        color: "1982c4",
        search_type: Some("school"),
        search_keyword: None,
    },
    PlaceType {
        name: "Gym",
        icon: "basketball",
        color: "ff595e",
        search_type: None,
        search_keyword: Some("gym"),
    },
    PlaceType {
        name: "Apartment",
        icon: "lodging",
        color: "2a9d8f",
        search_type: Some("apartment"),
        search_keyword: None,
    },
    PlaceType {
        name: "Martial arts school",
        icon: "fitness-centre",
        color: "8338ec",
        search_type: None,
        search_keyword: Some("martial arts"),
    },
    PlaceType {
        name: "School bus stop",
        icon: "bus",
        color: "c78d54",
        search_type: None,
        search_keyword: None,
    },
];

/// Creates an unpaid relocation for the account — one city, with the map's
/// center and zoom — and seeds its place types from [`SEED_PLACE_TYPES`].
/// Recording payment is a separate step, [`activate`], so a checkout
/// session can sit between the two.
pub async fn create(
    pool: &SqlitePool,
    account_id: i64,
    city_name: &str,
    lat: f64,
    lon: f64,
    zoom: f64,
) -> Result<i64, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let relocation_id: i64 = sqlx::query_scalar(
        "INSERT INTO relocations (account_id, city_name, lat, lon, zoom)
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(account_id)
    .bind(city_name)
    .bind(lat)
    .bind(lon)
    .bind(zoom)
    .fetch_one(&mut *transaction)
    .await?;
    for place_type in SEED_PLACE_TYPES {
        insert_place_type(&mut *transaction, relocation_id, place_type).await?;
    }
    transaction.commit().await?;
    Ok(relocation_id)
}

async fn insert_place_type<'e, E>(
    executor: E,
    relocation_id: i64,
    place_type: PlaceType<'_>,
) -> Result<i64, sqlx::Error>
where
    E: Executor<'e, Database = Sqlite>,
{
    let place_type_id: i64 = sqlx::query_scalar(
        "INSERT INTO place_types (relocation_id, name, icon, color, search_type, search_keyword)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(relocation_id)
    .bind(place_type.name)
    .bind(place_type.icon)
    .bind(place_type.color)
    .bind(place_type.search_type)
    .bind(place_type.search_keyword)
    .fetch_one(executor)
    .await?;
    Ok(place_type_id)
}
