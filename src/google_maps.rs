use std::{fmt, time::Duration};

use serde_json::Value;
use sqlx::SqlitePool;

use crate::{
    api_usage::{self, Provider},
    geo::Coordinates,
    http::{self, HttpResponse, HttpTransport},
};

const NEARBY_SEARCH_URL: &str = "https://maps.googleapis.com/maps/api/place/nearbysearch/json";
const FIND_PLACE_URL: &str = "https://maps.googleapis.com/maps/api/place/findplacefromtext/json";
const GEOCODE_URL: &str = "https://maps.googleapis.com/maps/api/geocode/json";
const DISTANCE_MATRIX_URL: &str = "https://maps.googleapis.com/maps/api/distancematrix/json";
/// The Places API v1 field mask the app stores with each place.
const DETAILS_FIELDS: &str =
    "businessStatus,location,id,plusCode,types,googleMapsUri,websiteUri,displayName";

/// The wait before asking for the next page, so the token Google just
/// issued is valid by then.
const NEXT_PAGE_DELAY: Duration = Duration::from_secs(2);
/// The wait before re-asking with a token Google just called invalid.
const PAGE_TOKEN_RETRY_DELAY: Duration = Duration::from_secs(1);
/// How often a not-yet-valid page token is retried before the search gives
/// up. The Rails app retried forever; a bound keeps a stuck job from
/// spinning.
const PAGE_TOKEN_RETRIES: u32 = 5;

/// Why a Google Maps call failed. Every variant is fit to show in a tool
/// error.
#[derive(Debug)]
pub enum GoogleMapsError {
    /// The request failed at the transport layer, after retries.
    Transport(http::TransportError),
    /// Counting the request in `api_usage` failed.
    Usage(sqlx::Error),
    /// The response body was not the JSON Google sends.
    Unparseable(serde_json::Error),
    /// The shared link did not answer HEAD with the expected 302.
    CantExpand(String),
    /// The expanded url names a place in no form the client reads.
    UnrecognisedLink(String),
    /// findplacefromtext did not answer exactly one candidate.
    AmbiguousPlace { name: String, candidates: usize },
    /// The single candidate carried no place_id.
    MissingPlaceId,
    /// The Places details endpoint answered a non-200 status.
    DetailsFailed(u16),
    /// The geocode endpoint answered a non-200 status.
    GeocodeFailed(u16),
    /// The geocode endpoint answered no result for the address.
    NoGeocodeResults(String),
    /// A next page token stayed INVALID_REQUEST across its retries.
    InvalidPageToken(String),
    /// The Distance Matrix endpoint answered a status other than OK.
    MatrixStatus(String),
    /// The Distance Matrix answer does not line up with the requested
    /// origins and destinations, so its entries cannot be matched to pairs.
    MatrixShape(String),
}

impl fmt::Display for GoogleMapsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "Google Maps request failed: {error}"),
            Self::Usage(error) => {
                write!(formatter, "counting the call in api_usage failed: {error}")
            }
            Self::Unparseable(error) => {
                write!(formatter, "Google Maps answered unparseable JSON: {error}")
            }
            Self::CantExpand(link) => write!(formatter, "can't expand {link}"),
            Self::UnrecognisedLink(url) => {
                write!(
                    formatter,
                    "expanded url does not match expected forms: {url}"
                )
            }
            Self::AmbiguousPlace { name, candidates } => {
                write!(formatter, "too many results for {name}: {candidates}")
            }
            Self::MissingPlaceId => {
                write!(
                    formatter,
                    "findplacefromtext returned a candidate without a place_id"
                )
            }
            Self::DetailsFailed(status) => {
                write!(formatter, "can't get details: Google answered {status}")
            }
            Self::GeocodeFailed(status) => {
                write!(
                    formatter,
                    "Google geocode request failed with status {status}"
                )
            }
            Self::NoGeocodeResults(address) => {
                write!(formatter, "no geocode results for {address}")
            }
            Self::InvalidPageToken(token) => {
                write!(
                    formatter,
                    "Google kept answering INVALID_REQUEST for page token {token}"
                )
            }
            Self::MatrixStatus(status) => {
                write!(formatter, "Google route matrix answered {status}")
            }
            Self::MatrixShape(mismatch) => write!(formatter, "{mismatch}"),
        }
    }
}

/// The travel modes the route matrix is asked for: the evaluation's bike,
/// transit and car times. Walking is not among them — the evaluation
/// derives walking minutes from stored isochrones, with no API call.
#[derive(Clone, Copy, Debug)]
pub enum TravelMode {
    Bicycling,
    Transit,
    Driving,
}

impl TravelMode {
    /// The Distance Matrix `mode` parameter value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bicycling => "bicycling",
            Self::Transit => "transit",
            Self::Driving => "driving",
        }
    }
}

impl std::error::Error for GoogleMapsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Usage(error) => Some(error),
            Self::Unparseable(error) => Some(error),
            _ => None,
        }
    }
}

/// The Google Maps client for one relocation: nearby search, place details,
/// geocoding and shared-link resolution through an injectable transport,
/// every request counted in `api_usage`.
pub struct GoogleMapsClient<'a> {
    pub transport: &'a dyn HttpTransport,
    pub pool: &'a SqlitePool,
    pub relocation_id: i64,
    pub api_key: &'a str,
}

impl GoogleMapsClient<'_> {
    /// Nearby search around the position within 50 km, following
    /// `next_page_token` to every page: each Google place as raw JSON, in
    /// page order. `search_type` is the Places type to look for,
    /// `keyword` the free text; an absent one is not sent.
    pub async fn find_by_type(
        &self,
        search_type: Option<&str>,
        center: Coordinates,
        keyword: Option<&str>,
    ) -> Result<Vec<Value>, GoogleMapsError> {
        let mut results = Vec::new();
        let mut page_token: Option<String> = None;
        let mut token_retries_left = PAGE_TOKEN_RETRIES;
        loop {
            let params = if let Some(token) = page_token.as_deref() {
                // A follow-up page carries only the token and the key.
                vec![
                    ("pagetoken", token.to_owned()),
                    ("key", self.api_key.to_owned()),
                ]
            } else {
                let mut params = vec![
                    ("radius", "50000".to_owned()),
                    ("location", format!("{},{}", center.lat, center.lng)),
                ];
                if let Some(search_type) = search_type {
                    params.push(("types", search_type.to_owned()));
                }
                if let Some(keyword) = keyword {
                    params.push(("keyword", keyword.to_owned()));
                }
                params.push(("key", self.api_key.to_owned()));
                params
            };
            let response = self.get(NEARBY_SEARCH_URL, &params).await?;
            let body: Value =
                serde_json::from_str(&response.body).map_err(GoogleMapsError::Unparseable)?;

            if let Some(token) = &page_token
                && body["status"] == "INVALID_REQUEST"
            {
                // Google issues a next_page_token a moment before it
                // becomes valid; an INVALID_REQUEST answer to a follow-up
                // means "not yet".
                if token_retries_left == 0 {
                    return Err(GoogleMapsError::InvalidPageToken(token.clone()));
                }
                token_retries_left -= 1;
                tokio::time::sleep(PAGE_TOKEN_RETRY_DELAY).await;
                continue;
            }

            if let Some(page_results) = body["results"].as_array() {
                results.extend(page_results.iter().cloned());
            }

            match body["next_page_token"].as_str() {
                Some(token) => {
                    page_token = Some(token.to_owned());
                    token_retries_left = PAGE_TOKEN_RETRIES;
                    // Wait for the token to become valid before asking.
                    tokio::time::sleep(NEXT_PAGE_DELAY).await;
                }
                None => return Ok(results),
            }
        }
    }

    /// The place id a shared Maps short link points at: expands the link
    /// with a HEAD that must answer 302, reads the place name and map
    /// centre out of the expanded url, and asks findplacefromtext — biased
    /// to the map centre when the url has one. Google must answer exactly
    /// one candidate.
    pub async fn place_id_from_shared_link(
        &self,
        shared_link: &str,
    ) -> Result<String, GoogleMapsError> {
        let expanded = self.expand_url(shared_link).await?;
        let (name, latlng) = extract_components(&expanded)?;
        self.find_place(&name, latlng.as_deref()).await
    }

    /// The Places API v1 details for the place id, with the field mask the
    /// app stores in `google_maps_payload`.
    pub async fn details_by_place_id(&self, place_id: &str) -> Result<Value, GoogleMapsError> {
        let url = format!(
            "https://places.googleapis.com/v1/places/{}",
            http::encode_component(place_id)
        );
        let response = self
            .get(
                &url,
                &[
                    ("fields", DETAILS_FIELDS.to_owned()),
                    ("key", self.api_key.to_owned()),
                ],
            )
            .await?;
        if response.status != 200 {
            return Err(GoogleMapsError::DetailsFailed(response.status));
        }
        serde_json::from_str(&response.body).map_err(GoogleMapsError::Unparseable)
    }

    /// Geocodes the address and returns the first result's
    /// `geometry.location`.
    pub async fn coordinates_from_address(
        &self,
        address: &str,
    ) -> Result<Coordinates, GoogleMapsError> {
        let response = self
            .get(
                GEOCODE_URL,
                &[
                    ("address", address.to_owned()),
                    ("key", self.api_key.to_owned()),
                ],
            )
            .await?;
        if response.status != 200 {
            return Err(GoogleMapsError::GeocodeFailed(response.status));
        }
        let body: Value =
            serde_json::from_str(&response.body).map_err(GoogleMapsError::Unparseable)?;
        let location = body["results"]
            .as_array()
            .and_then(|results| results.first())
            .map(|result| &result["geometry"]["location"]);
        let lat = location.and_then(|location| location["lat"].as_f64());
        let lng = location.and_then(|location| location["lng"].as_f64());
        match (lat, lng) {
            (Some(lat), Some(lng)) => Ok(Coordinates { lat, lng }),
            _ => Err(GoogleMapsError::NoGeocodeResults(address.to_owned())),
        }
    }

    /// One Distance Matrix request: the travel time in minutes from every
    /// origin to every destination by the mode, one row per origin and one
    /// entry per destination, in the order sent. An unreachable pair is
    /// None.
    pub async fn route_matrix(
        &self,
        origins: &[Coordinates],
        destinations: &[Coordinates],
        mode: TravelMode,
    ) -> Result<Vec<Vec<Option<u32>>>, GoogleMapsError> {
        let response = self
            .get(
                DISTANCE_MATRIX_URL,
                &[
                    ("origins", positions_param(origins)),
                    ("destinations", positions_param(destinations)),
                    ("mode", mode.as_str().to_owned()),
                    ("key", self.api_key.to_owned()),
                ],
            )
            .await?;
        let body: Value =
            serde_json::from_str(&response.body).map_err(GoogleMapsError::Unparseable)?;
        if body["status"] != "OK" {
            return Err(GoogleMapsError::MatrixStatus(
                body["status"].as_str().unwrap_or("unknown").to_owned(),
            ));
        }

        let rows: &[Value] = body["rows"].as_array().map_or(&[][..], Vec::as_slice);
        if rows.len() != origins.len() {
            return Err(GoogleMapsError::MatrixShape(format!(
                "route matrix answer does not match: {} rows for {} origins",
                rows.len(),
                origins.len()
            )));
        }
        let mut matrix = Vec::with_capacity(rows.len());
        for row in rows {
            let elements: &[Value] = row["elements"].as_array().map_or(&[][..], Vec::as_slice);
            if elements.len() != destinations.len() {
                return Err(GoogleMapsError::MatrixShape(format!(
                    "route matrix answer does not match: {} elements for {} destinations",
                    elements.len(),
                    destinations.len()
                )));
            }
            let durations = elements.iter().map(travel_minutes).collect();
            matrix.push(durations);
        }
        Ok(matrix)
    }

    /// Resolves a Maps short link to the url it points at. Short links
    /// answer HEAD with a 302; anything else means the link is not
    /// expandable.
    async fn expand_url(&self, shared_link: &str) -> Result<String, GoogleMapsError> {
        let response = self
            .transport
            .head(shared_link)
            .await
            .map_err(GoogleMapsError::Transport)?;
        if response.status != 302 {
            return Err(GoogleMapsError::CantExpand(shared_link.to_owned()));
        }
        response
            .location
            .ok_or_else(|| GoogleMapsError::CantExpand(shared_link.to_owned()))
    }

    /// findplacefromtext for the place name, biased to the map centre when
    /// there is one; returns the single candidate's place id.
    async fn find_place(
        &self,
        name: &str,
        latlng: Option<&str>,
    ) -> Result<String, GoogleMapsError> {
        let mut params = vec![
            ("input", name.to_owned()),
            ("fields", "place_id".to_owned()),
            ("inputtype", "textquery".to_owned()),
        ];
        if let Some(latlng) = latlng {
            params.push(("locationbias", format!("circle:10@{latlng}")));
        }
        params.push(("key", self.api_key.to_owned()));
        let response = self.get(FIND_PLACE_URL, &params).await?;
        let body: Value =
            serde_json::from_str(&response.body).map_err(GoogleMapsError::Unparseable)?;
        let candidates: &[Value] = body["candidates"].as_array().map_or(&[][..], Vec::as_slice);
        if candidates.len() != 1 {
            return Err(GoogleMapsError::AmbiguousPlace {
                name: name.to_owned(),
                candidates: candidates.len(),
            });
        }
        candidates[0]["place_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or(GoogleMapsError::MissingPlaceId)
    }

    /// One counted request: bumps `api_usage` first, then GETs through the
    /// transport-level retry.
    async fn get(
        &self,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<HttpResponse, GoogleMapsError> {
        api_usage::record_request(self.pool, self.relocation_id, Provider::Google)
            .await
            .map_err(GoogleMapsError::Usage)?;
        http::get_retrying(self.transport, url, params)
            .await
            .map_err(GoogleMapsError::Transport)
    }
}

/// The positions as a Distance Matrix parameter: "lat,lng" pairs joined
/// with `|`.
fn positions_param(positions: &[Coordinates]) -> String {
    positions
        .iter()
        .map(|position| format!("{},{}", position.lat, position.lng))
        .collect::<Vec<_>>()
        .join("|")
}

/// The element's travel duration in minutes, rounded to the nearest
/// minute, or None when the pair has no duration at all.
fn travel_minutes(element: &Value) -> Option<u32> {
    let seconds = element["duration"]["value"].as_u64()?;
    Some(((seconds + 30) / 60) as u32)
}

/// The place name and the "lat,lng" map centre the expanded url carries,
/// with the zoom stripped so the centre can bias a search. Two url forms
/// name a place: `maps.google.com/?q=<query>` and
/// `www.google.com/maps/place/<name>`.
fn extract_components(url: &str) -> Result<(String, Option<String>), GoogleMapsError> {
    if let Some(query) = url
        .strip_prefix("https://maps.google.com/?q=")
        .and_then(|rest| rest.split('&').next())
        .filter(|query| !query.is_empty())
    {
        let name = http::decode_component(query).replace('+', " ");
        // A q= url carries its centre inside the query itself.
        let centre = coordinates_in(&name);
        return Ok((name, centre));
    }
    if let Some(segment) = url
        .strip_prefix("https://www.google.com/maps/place/")
        .and_then(|rest| rest.split(['/', '?']).next())
        .filter(|segment| !segment.is_empty())
    {
        let name = http::decode_component(segment).replace('+', " ");
        return Ok((name, coordinates_in(url)));
    }
    Err(GoogleMapsError::UnrecognisedLink(url.to_owned()))
}

/// The map centre a Maps url points at: the "lat,lng" inside an
/// "@52.5,13.4,16.17z" segment, zoom stripped, or None when the url has no
/// centre.
fn coordinates_in(text: &str) -> Option<String> {
    text.match_indices('@')
        .find_map(|(at, _)| parse_centre(&text[at + '@'.len_utf8()..]))
}

/// Parses "lat,lng" followed by ",zoom z" right after an `@`, as the Rails
/// url regexp did.
fn parse_centre(after_at: &str) -> Option<String> {
    let mut parts = after_at.split(',');
    let lat = coordinate_token(parts.next()?)?;
    let lng = coordinate_token(parts.next()?)?;
    let zoom = parts.next()?;
    let zoom_digits = zoom
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .count();
    if zoom_digits > 0 && zoom.as_bytes().get(zoom_digits) == Some(&b'z') {
        Some(format!("{lat},{lng}"))
    } else {
        None
    }
}

/// A `-?[\d.]+` token: an optional minus, then digits and dots only.
fn coordinate_token(token: &str) -> Option<&str> {
    let digits = token.strip_prefix('-').unwrap_or(token);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        Some(token)
    } else {
        None
    }
}
