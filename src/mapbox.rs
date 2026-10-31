use std::{fmt, time::Duration};

use serde_json::Value;
use sqlx::SqlitePool;

use crate::{
    api_usage::{self, Caps, Provider},
    geo::Coordinates,
    http::{self, HttpResponse, HttpTransport},
};

/// The isochrone endpoint's own retry, from the Rails `get_isochrones!`:
/// three attempts in total, half a second between them, on any non-200.
const ISOCHRONE_ATTEMPTS: u32 = 3;
const ISOCHRONE_BACKOFF: Duration = Duration::from_millis(500);

/// Why a Mapbox call failed. Every variant is fit to show in a tool error.
#[derive(Debug)]
pub enum MapboxError {
    /// The request failed at the transport layer, after retries.
    Transport(http::TransportError),
    /// Counting the request in `api_usage` failed.
    Usage(sqlx::Error),
    /// A configured monthly cap refused the request, or reading the usage
    /// behind the check failed.
    Cap(api_usage::CapError),
    /// The response body was not the JSON Mapbox sends.
    Unparseable(serde_json::Error),
    /// Geocoding answered no feature for the address.
    NoGeocodeResults(String),
    /// The isochrone endpoint kept answering a non-200 status.
    IsochroneFailed(u16),
}

impl fmt::Display for MapboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "Mapbox request failed: {error}"),
            Self::Usage(error) => {
                write!(formatter, "counting the call in api_usage failed: {error}")
            }
            Self::Cap(error) => write!(formatter, "{error}"),
            Self::Unparseable(error) => {
                write!(formatter, "Mapbox answered unparseable JSON: {error}")
            }
            Self::NoGeocodeResults(address) => {
                write!(formatter, "no geocode results for {address}")
            }
            Self::IsochroneFailed(status) => {
                write!(formatter, "request to Mapbox failed: {status}")
            }
        }
    }
}

impl std::error::Error for MapboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Usage(error) => Some(error),
            Self::Cap(error) => Some(error),
            Self::Unparseable(error) => Some(error),
            _ => None,
        }
    }
}

/// The Mapbox client for one relocation: geocoding and isochrones through an
/// injectable transport, every request gated by the configured caps and
/// counted in `api_usage`.
pub struct MapboxClient<'a> {
    pub transport: &'a dyn HttpTransport,
    pub pool: &'a SqlitePool,
    pub relocation_id: i64,
    pub access_token: &'a str,
    pub caps: Caps,
}

impl MapboxClient<'_> {
    /// Geocodes an address, biased to the given position, and returns the
    /// center of the first feature Mapbox ranks. A `features` list without a
    /// usable first center is an error, not a zero position.
    pub async fn address_to_coordinates(
        &self,
        address: &str,
        near: Coordinates,
    ) -> Result<Coordinates, MapboxError> {
        let url = format!(
            "https://api.mapbox.com/geocoding/v5/mapbox.places/{}.json",
            http::encode_component(address)
        );
        let response = self
            .get(
                &url,
                &[
                    // The bias takes the position as lng,lat, Mapbox's order.
                    ("proximity", format!("{},{}", near.lng, near.lat)),
                    ("access_token", self.access_token.to_owned()),
                ],
            )
            .await?;
        let body: Value = serde_json::from_str(&response.body).map_err(MapboxError::Unparseable)?;
        let center = body["features"]
            .as_array()
            .and_then(|features| features.first())
            .and_then(|feature| feature["center"].as_array())
            .and_then(|center| {
                let lng = center.first()?.as_f64()?;
                let lat = center.get(1)?.as_f64()?;
                Some(Coordinates { lat, lng })
            });
        center.ok_or_else(|| MapboxError::NoGeocodeResults(address.to_owned()))
    }

    /// The walking isochrone polygons for `minutes` around the position, as
    /// Mapbox's GeoJSON. Non-200 answers are retried twice, as the Rails
    /// `get_isochrones!` did, before the status becomes the error.
    pub async fn isochrone(
        &self,
        position: Coordinates,
        minutes: u32,
    ) -> Result<Value, MapboxError> {
        let url = format!(
            "https://api.mapbox.com/isochrone/v1/mapbox/walking/{},{}",
            position.lng, position.lat
        );
        let params = [
            ("contours_minutes", minutes.to_string()),
            ("polygons", "true".to_owned()),
            ("access_token", self.access_token.to_owned()),
        ];
        let mut attempts_left = ISOCHRONE_ATTEMPTS;
        loop {
            let response = self.get(&url, &params).await?;
            if response.status == 200 {
                return serde_json::from_str(&response.body).map_err(MapboxError::Unparseable);
            }
            attempts_left -= 1;
            if attempts_left == 0 {
                return Err(MapboxError::IsochroneFailed(response.status));
            }
            tokio::time::sleep(ISOCHRONE_BACKOFF).await;
        }
    }

    /// One counted request: refuses past a configured cap, bumps
    /// `api_usage`, then GETs through the transport-level retry.
    async fn get(&self, url: &str, params: &[(&str, String)]) -> Result<HttpResponse, MapboxError> {
        api_usage::check_cap(self.pool, self.relocation_id, Provider::Mapbox, self.caps)
            .await
            .map_err(MapboxError::Cap)?;
        api_usage::record_request(self.pool, self.relocation_id, Provider::Mapbox)
            .await
            .map_err(MapboxError::Usage)?;
        http::get_retrying(self.transport, url, params)
            .await
            .map_err(MapboxError::Transport)
    }
}
