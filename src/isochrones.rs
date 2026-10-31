//! Walking isochrones: the contours generated for a location.

use std::fmt;

use sqlx::SqlitePool;

use crate::{
    geo::Coordinates,
    mapbox::{MapboxAccess, MapboxClient, MapboxError},
};

/// The walking-isochrone contours every location gets, in minutes.
pub const MINUTES: [i64; 5] = [5, 10, 15, 20, 30];

/// Why generating a location's isochrones failed. Every variant is fit
/// to show in a job's `last_error`.
#[derive(Debug)]
pub enum IsochroneError {
    /// No location has the given id.
    MissingLocation(i64),
    /// A Mapbox isochrone request failed.
    Mapbox(MapboxError),
    /// A database operation failed.
    Database(sqlx::Error),
}

impl From<sqlx::Error> for IsochroneError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<MapboxError> for IsochroneError {
    fn from(error: MapboxError) -> Self {
        Self::Mapbox(error)
    }
}

impl fmt::Display for IsochroneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLocation(location_id) => {
                write!(formatter, "no location exists with id {location_id}")
            }
            Self::Mapbox(error) => write!(formatter, "{error}"),
            Self::Database(error) => write!(formatter, "isochrone operation failed: {error}"),
        }
    }
}

impl std::error::Error for IsochroneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mapbox(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::MissingLocation(_) => None,
        }
    }
}

/// Generates the location's missing walking isochrones: every configured
/// [`MINUTES`] the location has no row for yet is fetched from Mapbox
/// and stored; the ones it already has are skipped, so a retry of a
/// half-finished generation only fetches the rest.
pub async fn generate_for_location(
    pool: &SqlitePool,
    mapbox: &MapboxAccess,
    location_id: i64,
) -> Result<(), IsochroneError> {
    let location: Option<(f64, f64, i64)> =
        sqlx::query_as("SELECT lat, lon, relocation_id FROM locations WHERE id = ?")
            .bind(location_id)
            .fetch_optional(pool)
            .await?;
    let Some((lat, lon, relocation_id)) = location else {
        return Err(IsochroneError::MissingLocation(location_id));
    };
    let client = MapboxClient {
        transport: &*mapbox.transport,
        pool,
        relocation_id,
        access_token: &mapbox.access_token,
        caps: mapbox.caps,
    };
    let existing: Vec<i64> =
        sqlx::query_scalar("SELECT minutes FROM isochrones WHERE location_id = ?")
            .bind(location_id)
            .fetch_all(pool)
            .await?;
    for minutes in MINUTES {
        if existing.contains(&minutes) {
            continue;
        }
        let geojson = client
            .isochrone(Coordinates { lat, lng: lon }, minutes as u32)
            .await?;
        sqlx::query("INSERT INTO isochrones (location_id, minutes, geojson) VALUES (?, ?, ?)")
            .bind(location_id)
            .bind(minutes)
            .bind(geojson.to_string())
            .execute(pool)
            .await?;
    }
    Ok(())
}
