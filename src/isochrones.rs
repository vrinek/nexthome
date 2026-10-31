//! Walking isochrones: the contours generated for a location, and the
//! heatmap layers the contours combine into.

use std::{collections::HashMap, fmt};

use geo::{
    BooleanOps, Coord, LineString, MultiPolygon, Orient, Polygon, orient::Direction, unary_union,
};
use serde_json::{Value, json};
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

/// One stored isochrone as the heatmap combines it: the contour's
/// minutes and its Mapbox GeoJSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Contour {
    pub minutes: i64,
    pub geojson: Value,
}

/// A heatmap: each layer's name over its GeoJSON FeatureCollection, in
/// the order the layers draw.
pub type Heatmap = Vec<(String, Value)>;

/// Why a heatmap could not be built.
#[derive(Debug)]
pub enum HeatmapError {
    /// A stored isochrone's geojson is not the JSON it should be.
    Unparseable(serde_json::Error),
    /// A database operation failed.
    Database(sqlx::Error),
}

impl From<sqlx::Error> for HeatmapError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for HeatmapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparseable(error) => {
                write!(
                    formatter,
                    "a stored isochrone's geojson is not valid JSON: {error}"
                )
            }
            Self::Database(error) => {
                write!(
                    formatter,
                    "reading the heatmap's isochrones failed: {error}"
                )
            }
        }
    }
}

impl std::error::Error for HeatmapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unparseable(error) => Some(error),
            Self::Database(error) => Some(error),
        }
    }
}

/// The solo heatmap's minute bands: the minutes whose union makes the
/// band, and the name the band draws under. The 15- and 25-minute
/// contours take part in no band — the Rails heatmap read only these
/// four, and this port keeps that.
const SOLO_BANDS: [(i64, &str); 4] = [(5, "0-5"), (10, "6-10"), (20, "11-20"), (30, "21-30")];

/// The minutes the intersection heatmap pairs, for both groups. The
/// Rails list carried 25 although generation never stored a 25-minute
/// contour, so those pairs intersect nothing; the port keeps the list.
const INTERSECTION_MINUTES: [i64; 6] = [5, 10, 15, 20, 25, 30];

/// The second group's minutes the intersection reads: the Rails matrix
/// had no column past 15, so pairs with a bigger second group were
/// skipped and their contours never take part.
const SECOND_MINUTES: [i64; 3] = [5, 10, 15];

/// The colour a minutes pair reads as, from the Rails matrix: the row
/// is the first group's minutes, the column the second group's among
/// [`SECOND_MINUTES`].
const COLOURS: [[&str; 3]; 6] = [
    ["blue", "blue", "green"],
    ["blue", "green", "green"],
    ["green", "green", "yellow"],
    ["green", "yellow", "yellow"],
    ["yellow", "yellow", "red"],
    ["yellow", "red", "red"],
];

/// The intersection heatmap's layers, best combination first: each
/// colour loses the next better one, so the bands do not overlap.
const COLOUR_ORDER: [&str; 4] = ["blue", "green", "yellow", "red"];

/// The heatmap of one group on its own: four minute bands, each the
/// union of the group's contours at the band's minutes with the next
/// smaller band subtracted, so the bands never overlap. The Rails
/// `solo_heatmap`, with its band names and its minutes.
pub fn solo_heatmap(group: &[Contour]) -> Heatmap {
    let mut layers: Heatmap = SOLO_BANDS
        .into_iter()
        .map(|(minutes, name)| (name.to_owned(), union_at(group, minutes)))
        .collect();
    subtract_the_next_smaller_band(&mut layers);
    layers
}

/// The heatmap of two groups combined: every pair of the groups'
/// minutes intersects the two per-minute unions and lands in the colour
/// the matrix gives it; each colour's intersections union into one
/// layer, and each layer loses the next better colour's. The Rails
/// `intersection_heatmap`, matrix and 15-minute cap on the second group
/// included.
pub fn intersection_heatmap(first: &[Contour], second: &[Contour]) -> Heatmap {
    // The per-minute unions, computed once each; the Rails cache stood
    // in for this.
    let first_unions: HashMap<i64, Value> = INTERSECTION_MINUTES
        .into_iter()
        .map(|minutes| (minutes, union_at(first, minutes)))
        .collect();
    let second_unions: HashMap<i64, Value> = SECOND_MINUTES
        .into_iter()
        .map(|minutes| (minutes, union_at(second, minutes)))
        .collect();

    let mut by_colour: HashMap<&str, Vec<Value>> = HashMap::new();
    for (row, first_minutes) in INTERSECTION_MINUTES.into_iter().enumerate() {
        for (column, second_minutes) in SECOND_MINUTES.into_iter().enumerate() {
            by_colour
                .entry(COLOURS[row][column])
                .or_default()
                .push(intersection(
                    &first_unions[&first_minutes],
                    &second_unions[&second_minutes],
                ));
        }
    }

    let mut layers: Heatmap = COLOUR_ORDER
        .into_iter()
        .map(|colour| {
            let pieces = by_colour.remove(colour).unwrap_or_default();
            let pieces = pieces.iter().collect::<Vec<_>>();
            (colour.to_owned(), union(&pieces))
        })
        .collect();
    subtract_the_next_smaller_band(&mut layers);
    layers
}

/// The heatmap of two place types' isochrones in the relocation: the
/// intersection heatmap when both sides have contours, and the same
/// colour layers empty when either side has none — a side without
/// isochrones leaves nothing to combine. The two place types are
/// parameters, where the Rails app hard-coded School and Gym.
pub async fn heatmap(
    pool: &SqlitePool,
    relocation_id: i64,
    first_place_type_id: i64,
    second_place_type_id: i64,
) -> Result<Heatmap, HeatmapError> {
    let first = contours_for(pool, relocation_id, first_place_type_id).await?;
    let second = contours_for(pool, relocation_id, second_place_type_id).await?;
    if first.is_empty() || second.is_empty() {
        return Ok(COLOUR_ORDER
            .into_iter()
            .map(|colour| (colour.to_owned(), empty_collection()))
            .collect());
    }
    Ok(intersection_heatmap(&first, &second))
}

/// The place type's stored contours in the relocation: every isochrone
/// of every location the type tracks.
async fn contours_for(
    pool: &SqlitePool,
    relocation_id: i64,
    place_type_id: i64,
) -> Result<Vec<Contour>, HeatmapError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT isochrones.minutes, isochrones.geojson
         FROM isochrones
         JOIN locations ON locations.id = isochrones.location_id
         WHERE locations.relocation_id = ? AND locations.place_type_id = ?",
    )
    .bind(relocation_id)
    .bind(place_type_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(minutes, geojson)| {
            serde_json::from_str(&geojson)
                .map(|geojson| Contour { minutes, geojson })
                .map_err(HeatmapError::Unparseable)
        })
        .collect()
}

/// Each layer loses the next smaller one, biggest pair first — the
/// Rails differencing ran the pairs reversed, so a layer is always cut
/// by the un-differenced band below it.
fn subtract_the_next_smaller_band(layers: &mut Heatmap) {
    for index in (1..layers.len()).rev() {
        let banded = difference(&layers[index].1, &layers[index - 1].1);
        layers[index].1 = banded;
    }
}

/// The union of the group's contours at exactly these minutes — the
/// Rails `Isochrone.merge` over its `where(minutes:)` scope.
fn union_at(group: &[Contour], minutes: i64) -> Value {
    union(
        &group
            .iter()
            .filter(|contour| contour.minutes == minutes)
            .map(|contour| &contour.geojson)
            .collect::<Vec<_>>(),
    )
}

/// The polygons of every collection in one layer: the port of the Rails
/// `Geojson.merge` feeding `Geojson.union` — concatenating features
/// only ever fed the union, so the port unions the polygons directly.
fn union(geojsons: &[&Value]) -> Value {
    feature_collection(unary_union(&polygons(geojsons)))
}

/// The polygons both collections share: the Rails `Geojson.intersect`.
/// A collection without polygons intersects to nothing.
fn intersection(first: &Value, second: &Value) -> Value {
    let (first_polygons, second_polygons) = (polygons(&[first]), polygons(&[second]));
    if first_polygons.is_empty() || second_polygons.is_empty() {
        return empty_collection();
    }
    feature_collection(MultiPolygon(first_polygons).intersection(&MultiPolygon(second_polygons)))
}

/// The polygons of `big` that `small` does not cover: the Rails
/// `Geojson.difference`. A `small` without polygons covers nothing, so
/// `big` stands as it is.
fn difference(big: &Value, small: &Value) -> Value {
    let small_polygons = polygons(&[small]);
    if small_polygons.is_empty() {
        return big.clone();
    }
    let big_polygons = polygons(&[big]);
    if big_polygons.is_empty() {
        return empty_collection();
    }
    feature_collection(MultiPolygon(big_polygons).difference(&MultiPolygon(small_polygons)))
}

/// Every polygon in the collections: the polygon and multi-polygon
/// features' rings, holes kept as holes and every ring wound the way
/// the set operations read — exterior counter-clockwise. Features of
/// any other geometry are skipped; Mapbox isochrones are polygons.
pub(crate) fn polygons(geojsons: &[&Value]) -> Vec<Polygon<f64>> {
    geojsons
        .iter()
        .flat_map(|geojson| {
            geojson["features"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|feature| &feature["geometry"])
        })
        .flat_map(geometry_polygons)
        .map(|polygon| polygon.orient(Direction::Default))
        .collect()
}

/// The geometry's polygons: a Polygon is one, a MultiPolygon is its
/// parts, anything else none.
fn geometry_polygons(geometry: &Value) -> Vec<Polygon<f64>> {
    match (
        geometry["type"].as_str(),
        geometry["coordinates"].as_array(),
    ) {
        (Some("Polygon"), Some(rings)) => polygon_from(rings).into_iter().collect(),
        (Some("MultiPolygon"), Some(polygons)) => polygons
            .iter()
            .filter_map(|polygon| polygon.as_array())
            .filter_map(|rings| polygon_from(rings))
            .collect(),
        _ => Vec::new(),
    }
}

/// The GeoJSON rings as one polygon, exterior ring first and holes
/// after. A ring that is not at least a closed triangle makes the whole
/// polygon none — a corrupt feature takes part in no layer rather than
/// half of one.
fn polygon_from(rings: &[Value]) -> Option<Polygon<f64>> {
    let (exterior, holes) = rings.split_first()?;
    Some(Polygon::new(
        ring_from(exterior.as_array()?)?,
        holes
            .iter()
            .map(|hole| ring_from(hole.as_array()?))
            .collect::<Option<Vec<_>>>()?,
    ))
}

/// The GeoJSON ring as a closed line string. Fewer than four positions
/// is not a ring, and a position that is not a pair of numbers is not a
/// position.
fn ring_from(ring: &[Value]) -> Option<LineString<f64>> {
    let coords = ring
        .iter()
        .map(|position| {
            Some(Coord {
                x: position.get(0)?.as_f64()?,
                y: position.get(1)?.as_f64()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let mut line = LineString(coords);
    if line.0.len() < 4 {
        return None;
    }
    line.close();
    Some(line)
}

/// The polygons as the geojson one heatmap layer carries: a single
/// feature with the polygons as its geometry — a Polygon when there is
/// one piece, a MultiPolygon when there are several — and no features
/// at all when the layer covers nothing.
fn feature_collection(polygons: MultiPolygon<f64>) -> Value {
    let geometry = match polygons.0.as_slice() {
        [] => return empty_collection(),
        [single] => json!({"type": "Polygon", "coordinates": polygon_coordinates(single)}),
        _ => json!({
            "type": "MultiPolygon",
            "coordinates": polygons.iter().map(polygon_coordinates).collect::<Vec<_>>(),
        }),
    };
    json!({
        "type": "FeatureCollection",
        "features": [{"type": "Feature", "geometry": geometry}],
    })
}

/// The polygon's rings as GeoJSON coordinates: the exterior first, then
/// the holes, each a closed ring of [lng, lat] positions.
fn polygon_coordinates(polygon: &Polygon<f64>) -> Value {
    Value::Array(
        std::iter::once(polygon.exterior())
            .chain(polygon.interiors())
            .map(|ring| {
                Value::Array(
                    ring.coords()
                        .map(|coord| Value::Array(vec![json!(coord.x), json!(coord.y)]))
                        .collect(),
                )
            })
            .collect(),
    )
}

/// The layer that covers nothing.
fn empty_collection() -> Value {
    json!({"type": "FeatureCollection", "features": []})
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
