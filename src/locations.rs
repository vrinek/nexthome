//! Locations: the places a relocation tracks, from the inbox where every
//! new place starts to the booked or rejected ends of the triage. The
//! functions here create, update, delete and list them — scoped to one
//! relocation, stamped with the member whose write created them, and
//! refused once the relocation is archived.

use std::{fmt, str::FromStr};

use serde_json::json;
use sqlx::{Executor, Sqlite, SqlitePool};

use crate::{
    geo::Coordinates,
    google_maps::{GoogleMapsClient, GoogleMapsError},
    jobs,
    mapbox::{MapboxClient, MapboxError},
    relocations::{self, RelocationError},
};

/// Where a location stands in the mover's triage. Every new location
/// starts in the inbox; the household then rejects or accepts it, and an
/// accepted apartment advances through contacted and waitlisted to
/// booked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Inbox,
    Rejected,
    Accepted,
    Contacted,
    Waitlisted,
    Booked,
}

impl State {
    /// The value stored in `locations.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Rejected => "rejected",
            Self::Accepted => "accepted",
            Self::Contacted => "contacted",
            Self::Waitlisted => "waitlisted",
            Self::Booked => "booked",
        }
    }

    /// The marker colour the state reads as. The Mapbox sprite's icons
    /// are flat images and cannot be tinted, so the map draws a coloured
    /// disc behind each one; a state must read the same wherever it
    /// shows up.
    pub fn color(self) -> &'static str {
        match self {
            Self::Inbox => "#1982c4",
            Self::Rejected => "#ff595e",
            Self::Accepted => "#8ac926",
            Self::Contacted => "#ffca3a",
            Self::Waitlisted => "#f77f00",
            Self::Booked => "#6a4c93",
        }
    }
}

impl FromStr for State {
    type Err = LocationError;

    /// The state a `locations.state` value names; the schema's CHECK
    /// constraint keeps stored values among these.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "inbox" => Ok(Self::Inbox),
            "rejected" => Ok(Self::Rejected),
            "accepted" => Ok(Self::Accepted),
            "contacted" => Ok(Self::Contacted),
            "waitlisted" => Ok(Self::Waitlisted),
            "booked" => Ok(Self::Booked),
            unknown => Err(LocationError::UnknownState(unknown.to_owned())),
        }
    }
}

/// Why a location operation was refused, or the failure behind one. The
/// refusal messages are fit to show to a user.
#[derive(Debug)]
pub enum LocationError {
    /// The relocation is archived; archived relocations are read-only.
    Archived,
    /// No relocation has the given id.
    RelocationNotFound,
    /// No location has the given id in the given relocation.
    LocationNotFound,
    /// No place type has the given id in the given relocation.
    PlaceTypeNotFound {
        place_type_id: i64,
        relocation_id: i64,
    },
    /// The member does not belong to the relocation's account, so their
    /// writes do not belong to the relocation.
    MemberNotInAccount { member_id: i64 },
    /// Neither provider geocoded the address within one degree of the
    /// relocation's city.
    TooFarFromCity { address: String, city: String },
    /// The Google place details carried no position.
    IncompleteDetails,
    /// Another location in the relocation already has the source url.
    DuplicateSourceUrl(String),
    /// Another location in the relocation already has the external
    /// identifier.
    DuplicateExternalIdentifier(String),
    /// A `locations.state` value that no state has.
    UnknownState(String),
    /// A Mapbox geocode failed.
    Mapbox(MapboxError),
    /// A Google Maps request failed.
    Google(GoogleMapsError),
    /// A database operation failed.
    Database(sqlx::Error),
}

impl fmt::Display for LocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archived => write!(formatter, "this relocation is archived and read-only"),
            Self::RelocationNotFound => write!(formatter, "no relocation exists with that id"),
            Self::LocationNotFound => {
                write!(
                    formatter,
                    "no location exists with that id in this relocation"
                )
            }
            Self::PlaceTypeNotFound {
                place_type_id,
                relocation_id,
            } => write!(
                formatter,
                "no place type {place_type_id} exists in relocation {relocation_id}"
            ),
            Self::MemberNotInAccount { member_id } => write!(
                formatter,
                "member {member_id} does not belong to the relocation's account"
            ),
            Self::TooFarFromCity { address, city } => {
                write!(
                    formatter,
                    "no geocode for {address} within one degree of {city}"
                )
            }
            Self::IncompleteDetails => {
                write!(formatter, "the Google place details carry no position")
            }
            Self::DuplicateSourceUrl(url) => write!(
                formatter,
                "another location in this relocation already has the source url {url}"
            ),
            Self::DuplicateExternalIdentifier(identifier) => write!(
                formatter,
                "another location in this relocation already has the external identifier {identifier}"
            ),
            Self::UnknownState(text) => write!(formatter, "{text} is not a location state"),
            Self::Mapbox(error) => write!(formatter, "{error}"),
            Self::Google(error) => write!(formatter, "{error}"),
            Self::Database(error) => write!(formatter, "location operation failed: {error}"),
        }
    }
}

impl std::error::Error for LocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mapbox(error) => Some(error),
            Self::Google(error) => Some(error),
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for LocationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<RelocationError> for LocationError {
    fn from(error: RelocationError) -> Self {
        match error {
            RelocationError::Archived => Self::Archived,
            RelocationError::NotFound => Self::RelocationNotFound,
            RelocationError::Database(error) => Self::Database(error),
        }
    }
}

impl From<MapboxError> for LocationError {
    fn from(error: MapboxError) -> Self {
        Self::Mapbox(error)
    }
}

impl From<GoogleMapsError> for LocationError {
    fn from(error: GoogleMapsError) -> Self {
        Self::Google(error)
    }
}

/// The fields of a new location that its placement does not supply.
#[derive(Clone, Copy)]
pub struct NewLocation<'a> {
    /// The location's name. A Google Maps link prefers the place
    /// details' display name and falls back to this.
    pub name: &'a str,
    /// One of the relocation's own place types.
    pub place_type_id: i64,
    pub note: Option<&'a str>,
    /// Where the location was found. A Google Maps link makes the link
    /// itself the source url.
    pub source_url: Option<&'a str>,
    pub placement: Placement<'a>,
}

/// How a new location's position is found.
#[derive(Clone, Copy)]
pub enum Placement<'a> {
    /// Geocode the address near the relocation's city: Mapbox first, and
    /// Google when Mapbox's answer lands too far away.
    Address(&'a str),
    /// The position is already known; no provider is called.
    Coordinates(Coordinates),
    /// Resolve the shared link to its place and take the name, position
    /// and place id from the place details.
    GoogleMapsLink(&'a str),
}

/// The relocation's city: the position that biases and bounds geocoding,
/// and the name the too-far refusal carries.
struct City {
    name: String,
    coordinates: Coordinates,
}

/// Creates a location in the relocation, stamped with the member who
/// added it. The placement decides how the position is found; the write
/// refuses archived or unknown relocations, place types and members from
/// outside the relocation, and a source url or external identifier the
/// relocation already tracks. Creating the location enqueues its
/// isochrones job.
pub async fn add(
    pool: &SqlitePool,
    relocation_id: i64,
    member_id: i64,
    mapbox: &MapboxClient<'_>,
    google: &GoogleMapsClient<'_>,
    new_location: NewLocation<'_>,
) -> Result<i64, LocationError> {
    // The city loads before any provider call, so an archived or unknown
    // relocation refuses the write before the write costs a request.
    let city = relocation_city(pool, relocation_id).await?;
    ensure_place_type(pool, relocation_id, new_location.place_type_id).await?;
    ensure_member(pool, relocation_id, member_id).await?;

    let resolved = match new_location.placement {
        Placement::Address(address) => {
            Resolved::at(geocode_near_city(mapbox, google, address, &city).await?)
        }
        Placement::Coordinates(coordinates) => Resolved::at(coordinates),
        Placement::GoogleMapsLink(link) => resolve_link(google, link).await?,
    };
    // A placement's own supplies win over the caller's: the place
    // details' display name, the link itself as the source url.
    let name = resolved.name.as_deref().unwrap_or(new_location.name);
    let source_url = resolved.source_url.as_deref().or(new_location.source_url);
    let external_identifier = resolved.external_identifier.as_deref();

    let mut transaction = pool.begin().await?;
    relocations::ensure_writable(&mut *transaction, relocation_id).await?;
    if let Some(source_url) = source_url {
        ensure_unique_source_url(&mut *transaction, relocation_id, source_url).await?;
    }
    if let Some(external_identifier) = external_identifier {
        ensure_unique_external_identifier(&mut *transaction, relocation_id, external_identifier)
            .await?;
    }
    let location_id: i64 = sqlx::query_scalar(
        "INSERT INTO locations
            (relocation_id, place_type_id, member_id, name, lat, lon, state, note,
             source_url, external_identifier, google_maps_payload, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                 strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         RETURNING id",
    )
    .bind(relocation_id)
    .bind(new_location.place_type_id)
    .bind(member_id)
    .bind(name)
    .bind(resolved.coordinates.lat)
    .bind(resolved.coordinates.lng)
    .bind(State::Inbox.as_str())
    .bind(new_location.note)
    .bind(source_url)
    .bind(external_identifier)
    .bind(resolved.google_maps_payload.as_deref())
    .fetch_one(&mut *transaction)
    .await?;
    jobs::enqueue_isochrones(&mut *transaction, location_id).await?;
    transaction.commit().await?;
    Ok(location_id)
}

/// The fields an update may change; each None leaves the field as it
/// stands.
#[derive(Default)]
pub struct LocationChanges<'a> {
    pub state: Option<State>,
    pub note: Option<&'a str>,
    pub name: Option<&'a str>,
    pub coordinates: Option<Coordinates>,
}

/// Updates a location's state, note, name or position. Moving it — new
/// coordinates — re-enqueues its isochrones job and deletes its stored
/// evaluation, which the move has invalidated. The write goes through
/// the archive guard and stays scoped: a location of another relocation
/// is not found.
pub async fn update(
    pool: &SqlitePool,
    relocation_id: i64,
    location_id: i64,
    changes: LocationChanges<'_>,
) -> Result<(), LocationError> {
    let moved = changes.coordinates.is_some();
    let mut transaction = pool.begin().await?;
    relocations::ensure_writable(&mut *transaction, relocation_id).await?;

    let mut assignments = vec!["updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')"];
    if changes.state.is_some() {
        assignments.push("state = ?");
    }
    if changes.note.is_some() {
        assignments.push("note = ?");
    }
    if changes.name.is_some() {
        assignments.push("name = ?");
    }
    if moved {
        assignments.extend(["lat = ?", "lon = ?"]);
    }
    let sql = format!(
        "UPDATE locations SET {} WHERE id = ? AND relocation_id = ?",
        assignments.join(", ")
    );
    let mut query = sqlx::query(&sql);
    if let Some(state) = changes.state {
        query = query.bind(state.as_str());
    }
    if let Some(note) = changes.note {
        query = query.bind(note);
    }
    if let Some(name) = changes.name {
        query = query.bind(name);
    }
    if let Some(coordinates) = changes.coordinates {
        query = query.bind(coordinates.lat).bind(coordinates.lng);
    }
    let result = query
        .bind(location_id)
        .bind(relocation_id)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() == 0 {
        return Err(LocationError::LocationNotFound);
    }

    if moved {
        jobs::enqueue_isochrones(&mut *transaction, location_id).await?;
        sqlx::query("DELETE FROM evaluations WHERE location_id = ?")
            .bind(location_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Deletes a location, with the isochrones and the evaluation that hang
/// off it, through the archive guard and scoped to the relocation.
pub async fn delete(
    pool: &SqlitePool,
    relocation_id: i64,
    location_id: i64,
) -> Result<(), LocationError> {
    let mut transaction = pool.begin().await?;
    relocations::ensure_writable(&mut *transaction, relocation_id).await?;
    sqlx::query("DELETE FROM evaluations WHERE location_id = ?")
        .bind(location_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM isochrones WHERE location_id = ?")
        .bind(location_id)
        .execute(&mut *transaction)
        .await?;
    let result = sqlx::query("DELETE FROM locations WHERE id = ? AND relocation_id = ?")
        .bind(location_id)
        .bind(relocation_id)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() == 0 {
        return Err(LocationError::LocationNotFound);
    }
    transaction.commit().await?;
    Ok(())
}

/// The relocation's city, or the refusal for an unknown or archived
/// relocation.
async fn relocation_city(pool: &SqlitePool, relocation_id: i64) -> Result<City, LocationError> {
    let row: Option<(String, f64, f64, Option<String>)> =
        sqlx::query_as("SELECT city_name, lat, lon, archived_at FROM relocations WHERE id = ?")
            .bind(relocation_id)
            .fetch_optional(pool)
            .await?;
    match row {
        None => Err(LocationError::RelocationNotFound),
        Some((_, _, _, Some(_))) => Err(LocationError::Archived),
        Some((name, lat, lon, None)) => Ok(City {
            name,
            coordinates: Coordinates { lat, lng: lon },
        }),
    }
}

/// Refuses the write when the place type belongs to no relocation or a
/// different one: a location's place type must be one its own relocation
/// tracks.
async fn ensure_place_type(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type_id: i64,
) -> Result<(), LocationError> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM place_types WHERE id = ? AND relocation_id = ?")
            .bind(place_type_id)
            .bind(relocation_id)
            .fetch_optional(pool)
            .await?;
    if found.is_none() {
        return Err(LocationError::PlaceTypeNotFound {
            place_type_id,
            relocation_id,
        });
    }
    Ok(())
}

/// Refuses the write when the member does not belong to the relocation's
/// account: members write only within their own account's relocations.
async fn ensure_member(
    pool: &SqlitePool,
    relocation_id: i64,
    member_id: i64,
) -> Result<(), LocationError> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT members.id FROM members
         JOIN relocations ON relocations.account_id = members.account_id
         WHERE members.id = ? AND relocations.id = ?",
    )
    .bind(member_id)
    .bind(relocation_id)
    .fetch_optional(pool)
    .await?;
    if found.is_none() {
        return Err(LocationError::MemberNotInAccount { member_id });
    }
    Ok(())
}

/// What a placement resolved to: the position every placement supplies,
/// plus the fields only a Google Maps link does.
struct Resolved {
    coordinates: Coordinates,
    /// The place details' display name; a placement that supplies none
    /// leaves the caller's name standing.
    name: Option<String>,
    /// The place id the link resolved to, as the stable identifier the
    /// Rails app built from it.
    external_identifier: Option<String>,
    /// The place details, stored as the location's Google Maps payload.
    google_maps_payload: Option<String>,
    /// The source url the placement itself sets: the link.
    source_url: Option<String>,
}

impl Resolved {
    /// A placement that supplied only its position.
    fn at(coordinates: Coordinates) -> Self {
        Self {
            coordinates,
            name: None,
            external_identifier: None,
            google_maps_payload: None,
            source_url: None,
        }
    }
}

/// The Rails `set_coordinates_from_address`: Mapbox geocodes the address
/// biased to the city, and an answer more than one degree from the city,
/// on either axis, goes to Google instead; a Google answer equally far
/// refuses the address — a place was found, but not one in the
/// relocation's city.
async fn geocode_near_city(
    mapbox: &MapboxClient<'_>,
    google: &GoogleMapsClient<'_>,
    address: &str,
    city: &City,
) -> Result<Coordinates, LocationError> {
    let from_mapbox = mapbox
        .address_to_coordinates(address, city.coordinates)
        .await?;
    if within_one_degree(city.coordinates, from_mapbox) {
        return Ok(from_mapbox);
    }
    let from_google = google.coordinates_from_address(address).await?;
    if within_one_degree(city.coordinates, from_google) {
        return Ok(from_google);
    }
    Err(LocationError::TooFarFromCity {
        address: address.to_owned(),
        city: city.name.clone(),
    })
}

/// Whether the position sits within one degree of the city on both axes.
fn within_one_degree(city: Coordinates, position: Coordinates) -> bool {
    (city.lat - position.lat).abs() <= 1.0 && (city.lng - position.lng).abs() <= 1.0
}

/// Resolves a shared Google Maps link to the place it names: the link's
/// place id, then the place's details, whose display name and position
/// fill the location. The details are stored whole as the Google Maps
/// payload, stamped version 2 — the payload format this port reads — and
/// the place id becomes the external identifier.
async fn resolve_link(
    google: &GoogleMapsClient<'_>,
    link: &str,
) -> Result<Resolved, LocationError> {
    let place_id = google.place_id_from_shared_link(link).await?;
    let details = google.details_by_place_id(&place_id).await?;
    let lat = details["location"]["latitude"].as_f64();
    let lng = details["location"]["longitude"].as_f64();
    let coordinates = match (lat, lng) {
        (Some(lat), Some(lng)) => Coordinates {
            lat: round_to_six_decimals(lat),
            lng: round_to_six_decimals(lng),
        },
        _ => return Err(LocationError::IncompleteDetails),
    };
    let mut payload = details.clone();
    payload["version"] = json!(2);
    Ok(Resolved {
        coordinates,
        name: details["displayName"]["text"].as_str().map(str::to_owned),
        external_identifier: Some(format!("google_maps:place_id:{place_id}")),
        google_maps_payload: Some(payload.to_string()),
        source_url: Some(link.to_owned()),
    })
}

/// The value rounded to six decimals, the precision the Rails app stored
/// Google positions at.
fn round_to_six_decimals(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Refuses the write when the relocation already tracks a location with
/// the source url: source urls are unique within a relocation, and a
/// missing one never collides.
async fn ensure_unique_source_url<'e, E>(
    executor: E,
    relocation_id: i64,
    source_url: &str,
) -> Result<(), LocationError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM locations WHERE relocation_id = ? AND source_url = ?")
            .bind(relocation_id)
            .bind(source_url)
            .fetch_optional(executor)
            .await?;
    if existing.is_some() {
        return Err(LocationError::DuplicateSourceUrl(source_url.to_owned()));
    }
    Ok(())
}

/// Refuses the write when the relocation already tracks a location with
/// the external identifier: external identifiers are unique within a
/// relocation, and a missing one never collides.
async fn ensure_unique_external_identifier<'e, E>(
    executor: E,
    relocation_id: i64,
    external_identifier: &str,
) -> Result<(), LocationError>
where
    E: Executor<'e, Database = Sqlite>,
{
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM locations WHERE relocation_id = ? AND external_identifier = ?",
    )
    .bind(relocation_id)
    .bind(external_identifier)
    .fetch_optional(executor)
    .await?;
    if existing.is_some() {
        return Err(LocationError::DuplicateExternalIdentifier(
            external_identifier.to_owned(),
        ));
    }
    Ok(())
}
