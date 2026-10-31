//! Locations: the places a relocation tracks, from the inbox where every
//! new place starts to the booked or rejected ends of the triage. The
//! functions here create, update, delete and list them — scoped to one
//! relocation, stamped with the member whose write created them, and
//! refused once the relocation is archived.

use std::{fmt, str::FromStr};

use crate::{google_maps::GoogleMapsError, mapbox::MapboxError, relocations::RelocationError};

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
