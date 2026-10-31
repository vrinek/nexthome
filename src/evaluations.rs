//! Evaluations: the travel-time table for one location — per place type
//! the three nearest accepted places, each with walking minutes from
//! point-in-polygon against the destination's stored isochrones and
//! bike, transit and car minutes from a batched Google route-matrix
//! request — stored in `evaluations` and returned as the plan's text
//! table.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

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
