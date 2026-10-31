//! Evaluations: the travel-time table for one location — per place type
//! the three nearest accepted places, each with walking minutes from
//! point-in-polygon against the destination's stored isochrones and
//! bike, transit and car minutes from a batched Google route-matrix
//! request — stored in `evaluations` and returned as the plan's text
//! table.

use std::fmt::{self, Write as _};

use geo::{Distance, Haversine, Point};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    geo::Coordinates,
    google_maps::{GoogleMapsClient, GoogleMapsError, TravelMode},
};

/// One row of a stored evaluation: one of a place type's nearest
/// accepted places, and the travel minutes to it by each mode. A mode
/// the table has no time for — a position beyond the walking contours,
/// a pair the matrix cannot route — is None, which renders as "-".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRow {
    /// The place type's name, as the table labels the row.
    pub place_type: String,
    /// The destination location's id.
    pub destination_location_id: i64,
    /// The destination's name, as the table labels the row.
    pub destination: String,
    /// Walking minutes: the smallest walking isochrone of the
    /// destination that contains the evaluated position.
    pub walk: Option<u32>,
    pub bike: Option<u32>,
    pub transit: Option<u32>,
    pub car: Option<u32>,
}

/// The minutes within which a row renders: a place shows when any mode
/// reaches it within these, and a place type whose rows all fail it —
/// or that has none — renders the plan's "none within 30 min by any
/// mode" line instead.
const WITHIN_MINUTES: u32 = 30;

/// The line a place type with nothing to show renders, from the plan's
/// example table.
const NONE_WITHIN: &str = "none within 30 min by any mode";

/// The table's label column, from the plan's example: the location's
/// name and the "type: destination" labels left-align here, and every
/// mode column — two spaces in front of it — follows, so the first
/// column begins where the plan's example begins it.
const LABEL_WIDTH: usize = 27;

/// The table's mode columns: the header word and the width it
/// right-aligns in, the minutes under it.
const MODE_COLUMNS: [(&str, usize); 4] = [("walk", 4), ("bike", 4), ("transit", 7), ("car", 3)];

/// The evaluation as text, in the plan's format: the location's name
/// over the four mode columns, one line per place that some mode
/// reaches within [`WITHIN_MINUTES`], in place-type order with each
/// type's nearest places first, and the "none within" line for a place
/// type nothing renders for.
pub fn render(location_name: &str, place_types: &[&str], rows: &[EvaluationRow]) -> String {
    let mut lines = vec![header_line(location_name)];
    for place_type in place_types {
        let rendered = rows
            .iter()
            .filter(|row| row.place_type == *place_type && row.renders())
            .collect::<Vec<_>>();
        if rendered.is_empty() {
            lines.push(format!("{place_type}: {NONE_WITHIN}"));
            continue;
        }
        for row in rendered {
            lines.push(place_line(place_type, row));
        }
    }
    lines.join("\n")
}

impl EvaluationRow {
    /// Whether the row renders: some mode reaches the destination
    /// within [`WITHIN_MINUTES`], the condition the "none within" line
    /// stands in for when no row of a place type meets it.
    fn renders(&self) -> bool {
        [self.walk, self.bike, self.transit, self.car]
            .into_iter()
            .flatten()
            .any(|minutes| minutes <= WITHIN_MINUTES)
    }
}

/// The table's header line: the location's name over the mode columns.
fn header_line(location_name: &str) -> String {
    let mut line = format!("{:<width$}", location_name, width = LABEL_WIDTH);
    for (mode, width) in MODE_COLUMNS {
        push_column(&mut line, mode, width);
    }
    line
}

/// One place's line: the type and destination as the label, then the
/// four mode columns, "-" where a mode has no time.
fn place_line(place_type: &str, row: &EvaluationRow) -> String {
    let label = format!("{place_type}: {}", row.destination);
    let mut line = format!("{:<width$}", label, width = LABEL_WIDTH);
    for ((_, width), minutes) in MODE_COLUMNS
        .iter()
        .zip([row.walk, row.bike, row.transit, row.car])
    {
        let text = minutes
            .map(|minutes| minutes.to_string())
            .unwrap_or_else(|| "-".to_owned());
        push_column(&mut line, &text, *width);
    }
    line
}

/// One mode column onto a line: the text right-aligned in its width,
/// two spaces in front, as the plan's example spaces its columns.
fn push_column(line: &mut String, text: &str, width: usize) {
    write!(line, "  {:>width$}", text, width = width).expect("writing to a String cannot fail");
}

/// Why an evaluation failed. Every variant is fit to show in a tool
/// error.
#[derive(Debug)]
pub enum EvaluationError {
    /// No location has the given id.
    MissingLocation(i64),
    /// A Google Maps request behind the evaluation failed.
    Google(GoogleMapsError),
    /// A stored evaluation's rows are not the JSON this module writes.
    Unparseable(serde_json::Error),
    /// A database operation failed.
    Database(sqlx::Error),
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLocation(location_id) => {
                write!(formatter, "no location exists with id {location_id}")
            }
            Self::Google(error) => write!(formatter, "{error}"),
            Self::Unparseable(error) => {
                write!(
                    formatter,
                    "a stored evaluation's rows are not valid JSON: {error}"
                )
            }
            Self::Database(error) => write!(formatter, "evaluation operation failed: {error}"),
        }
    }
}

impl std::error::Error for EvaluationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Google(error) => Some(error),
            Self::Unparseable(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::MissingLocation(_) => None,
        }
    }
}

impl From<sqlx::Error> for EvaluationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<GoogleMapsError> for EvaluationError {
    fn from(error: GoogleMapsError) -> Self {
        Self::Google(error)
    }
}

/// The location's travel-time table as text, computed on the first
/// call and read back from `evaluations` after: per place type, the
/// three nearest accepted places with their minutes by each mode.
pub async fn evaluate(
    pool: &SqlitePool,
    google: &GoogleMapsClient<'_>,
    location_id: i64,
) -> Result<String, EvaluationError> {
    let location = location_by_id(pool, location_id).await?;
    let place_types = place_types(pool, location.relocation_id).await?;
    if let Some(rows) = stored_rows(pool, location_id).await? {
        return Ok(render(&location.name, &names(&place_types), &rows));
    }
    let rows = compute(pool, google, &location, &place_types).await?;
    store(pool, location_id, &rows).await?;
    Ok(render(&location.name, &names(&place_types), &rows))
}

/// The location an evaluation is for: the relocation whose place types
/// and accepted places the table covers, the name the table heads it
/// with, and the position everything is measured from.
struct EvaluatedLocation {
    id: i64,
    relocation_id: i64,
    name: String,
    coordinates: Coordinates,
}

/// One of a place type's nearest accepted places, as the evaluation
/// computes it.
struct NearestPlace {
    id: i64,
    name: String,
    coordinates: Coordinates,
}

/// The evaluation's rows: per place type, its nearest accepted places —
/// the three nearest by straight-line distance, the evaluated location
/// itself never one of them — each carrying the bike, transit and car
/// minutes one batched matrix request per mode answers for them all.
async fn compute(
    pool: &SqlitePool,
    google: &GoogleMapsClient<'_>,
    location: &EvaluatedLocation,
    place_types: &[(i64, String)],
) -> Result<Vec<EvaluationRow>, EvaluationError> {
    let mut entries = Vec::new();
    for (place_type_id, place_type) in place_types {
        for place in nearest_accepted(
            pool,
            location.relocation_id,
            *place_type_id,
            location.id,
            location.coordinates,
        )
        .await?
        {
            entries.push((place_type.as_str(), place));
        }
    }
    let destinations: Vec<Coordinates> =
        entries.iter().map(|(_, place)| place.coordinates).collect();
    let matrix = if destinations.is_empty() {
        Vec::new()
    } else {
        matrix_minutes(google, location.coordinates, &destinations).await?
    };
    let mut rows = Vec::with_capacity(entries.len());
    for ((place_type, place), minutes) in entries.iter().zip(matrix) {
        rows.push(EvaluationRow {
            place_type: (*place_type).to_owned(),
            destination_location_id: place.id,
            destination: place.name.clone(),
            walk: None,
            bike: minutes.bike,
            transit: minutes.transit,
            car: minutes.car,
        });
    }
    Ok(rows)
}

/// The place type's three nearest accepted places to the position, by
/// straight-line distance, nearest first, the evaluated location
/// itself never among them; a type with fewer than three accepted
/// places takes them all.
async fn nearest_accepted(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type_id: i64,
    evaluated_location_id: i64,
    from: Coordinates,
) -> Result<Vec<NearestPlace>, EvaluationError> {
    let rows: Vec<(i64, String, f64, f64)> = sqlx::query_as(
        "SELECT id, name, lat, lon FROM locations
         WHERE relocation_id = ? AND place_type_id = ? AND state = 'accepted' AND id != ?
         ORDER BY id",
    )
    .bind(relocation_id)
    .bind(place_type_id)
    .bind(evaluated_location_id)
    .fetch_all(pool)
    .await?;
    let mut places = rows
        .into_iter()
        .map(|(id, name, lat, lon)| {
            let coordinates = Coordinates { lat, lng: lon };
            (
                Haversine.distance(point(from), point(coordinates)),
                NearestPlace {
                    id,
                    name,
                    coordinates,
                },
            )
        })
        .collect::<Vec<_>>();
    places.sort_by(|(distance, _), (other, _)| distance.total_cmp(other));
    Ok(places.into_iter().take(3).map(|(_, place)| place).collect())
}

/// The bike, transit and car minutes to every destination: one batched
/// Distance Matrix request per mode — the endpoint answers one mode at
/// a time — with every destination in it, so the evaluation costs three
/// requests however many places it covers.
async fn matrix_minutes(
    google: &GoogleMapsClient<'_>,
    origin: Coordinates,
    destinations: &[Coordinates],
) -> Result<Vec<MatrixMinutes>, EvaluationError> {
    let mut by_mode = Vec::new();
    for mode in [
        TravelMode::Bicycling,
        TravelMode::Transit,
        TravelMode::Driving,
    ] {
        let matrix = google.route_matrix(&[origin], destinations, mode).await?;
        by_mode.push(
            matrix
                .into_iter()
                .next()
                .expect("the matrix answers one row per origin"),
        );
    }
    Ok((0..destinations.len())
        .map(|index| MatrixMinutes {
            bike: by_mode[0][index],
            transit: by_mode[1][index],
            car: by_mode[2][index],
        })
        .collect())
}

/// The bike, transit and car minutes one destination's matrix answers
/// carry, by mode.
#[derive(Clone, Copy)]
struct MatrixMinutes {
    bike: Option<u32>,
    transit: Option<u32>,
    car: Option<u32>,
}

/// The location's stored evaluation rows, when one stands: the cache
/// the first call filled.
async fn stored_rows(
    pool: &SqlitePool,
    location_id: i64,
) -> Result<Option<Vec<EvaluationRow>>, EvaluationError> {
    let rows: Option<String> =
        sqlx::query_scalar("SELECT rows FROM evaluations WHERE location_id = ?")
            .bind(location_id)
            .fetch_optional(pool)
            .await?;
    match rows {
        None => Ok(None),
        Some(rows) => serde_json::from_str(&rows)
            .map(Some)
            .map_err(EvaluationError::Unparseable),
    }
}

/// Stores the evaluation as the location's one row in `evaluations`,
/// replacing whatever row stood for the location, stamped with the
/// moment it computed.
async fn store(
    pool: &SqlitePool,
    location_id: i64,
    rows: &[EvaluationRow],
) -> Result<(), EvaluationError> {
    let rows = serde_json::to_string(rows).expect("evaluation rows serialize as JSON");
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM evaluations WHERE location_id = ?")
        .bind(location_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO evaluations (location_id, computed_at, rows)
         VALUES (?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), ?)",
    )
    .bind(location_id)
    .bind(rows)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// The location an evaluation is for, or the refusal when none has the
/// id.
async fn location_by_id(
    pool: &SqlitePool,
    location_id: i64,
) -> Result<EvaluatedLocation, EvaluationError> {
    let row: Option<(i64, String, f64, f64)> =
        sqlx::query_as("SELECT relocation_id, name, lat, lon FROM locations WHERE id = ?")
            .bind(location_id)
            .fetch_optional(pool)
            .await?;
    row.map(|(relocation_id, name, lat, lon)| EvaluatedLocation {
        id: location_id,
        relocation_id,
        name,
        coordinates: Coordinates { lat, lng: lon },
    })
    .ok_or(EvaluationError::MissingLocation(location_id))
}

/// The relocation's place types in the order they were seeded, which is
/// the order the table lists them in.
async fn place_types(
    pool: &SqlitePool,
    relocation_id: i64,
) -> Result<Vec<(i64, String)>, EvaluationError> {
    Ok(
        sqlx::query_as("SELECT id, name FROM place_types WHERE relocation_id = ? ORDER BY id")
            .bind(relocation_id)
            .fetch_all(pool)
            .await?,
    )
}

/// The place types' names, in the order the table lists them in.
fn names(place_types: &[(i64, String)]) -> Vec<&str> {
    place_types.iter().map(|(_, name)| name.as_str()).collect()
}

/// The position as the `geo` crate reads it: x the longitude, y the
/// latitude, the order GeoJSON coordinates carry.
fn point(coordinates: Coordinates) -> Point<f64> {
    Point::new(coordinates.lng, coordinates.lat)
}
