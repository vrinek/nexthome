use std::fmt;

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

/// A relocation after activation: the instant payment was recorded and
/// the instant, six calendar months later, when its data turns read-only.
#[derive(Debug)]
pub struct Activated {
    pub paid_at: String,
    pub expires_at: String,
}

/// Why a relocation write was refused, or the database error behind a
/// failed one. The refusal messages are fit to show to a user.
#[derive(Debug)]
pub enum RelocationError {
    /// The relocation is archived; archived relocations are read-only.
    Archived,
    /// No relocation has the given id.
    NotFound,
    /// The database operation failed.
    Database(sqlx::Error),
}

impl From<sqlx::Error> for RelocationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for RelocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archived => write!(formatter, "this relocation is archived and read-only"),
            Self::NotFound => write!(formatter, "no relocation exists with that id"),
            Self::Database(error) => write!(formatter, "relocation write failed: {error}"),
        }
    }
}

impl std::error::Error for RelocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

/// The write guard: every mutation of a relocation's data calls this first.
/// Loads the row and refuses the write when the relocation is archived —
/// archived relocations are read-only — or when no row has the id.
pub async fn ensure_writable<'e, E>(executor: E, relocation_id: i64) -> Result<(), RelocationError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let archived_at: Option<Option<String>> =
        sqlx::query_scalar("SELECT archived_at FROM relocations WHERE id = ?")
            .bind(relocation_id)
            .fetch_optional(executor)
            .await?;
    match archived_at {
        None => Err(RelocationError::NotFound),
        Some(Some(_)) => Err(RelocationError::Archived),
        Some(None) => Ok(()),
    }
}

/// Adds a place type to a relocation beyond the seeded ones, through the
/// write guard: an archived relocation refuses it.
pub async fn add_place_type(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type: PlaceType<'_>,
) -> Result<i64, RelocationError> {
    let mut transaction = pool.begin().await?;
    ensure_writable(&mut *transaction, relocation_id).await?;
    let place_type_id = insert_place_type(&mut *transaction, relocation_id, place_type).await?;
    transaction.commit().await?;
    Ok(place_type_id)
}

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

/// Marks the relocation paid: `paid_at` becomes now and `expires_at` the
/// same instant advanced by six calendar months. Both timestamps are
/// written by one UPDATE, so they derive from the same second. Refuses
/// archived or unknown relocations through the write guard.
pub async fn activate(pool: &SqlitePool, relocation_id: i64) -> Result<Activated, RelocationError> {
    let mut transaction = pool.begin().await?;
    ensure_writable(&mut *transaction, relocation_id).await?;
    let (paid_at, expires_at): (String, String) = sqlx::query_as(
        "UPDATE relocations
         SET paid_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             expires_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+6 months')
         WHERE id = ?
         RETURNING paid_at, expires_at",
    )
    .bind(relocation_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Activated {
        paid_at,
        expires_at,
    })
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
