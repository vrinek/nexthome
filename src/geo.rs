/// A position on the map: the latitude/longitude pair the map clients and
/// the relocation data share.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates {
    pub lat: f64,
    pub lng: f64,
}
