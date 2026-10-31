//! Evaluations: the travel-time table for one location — per place type
//! the three nearest accepted places, each with walking minutes from
//! point-in-polygon against the destination's stored isochrones and
//! bike, transit and car minutes from a batched Google route-matrix
//! request — computed on the first call, cached in `evaluations` after,
//! invalidated by the two writes that make it stale, and rendered as
//! the plan's text table. No test calls a live provider.

mod common;

use nexthome::evaluations::{EvaluationRow, render};

#[test]
fn the_table_renders_in_the_plan_s_format() {
    // The plan's own example table: the Work row renders although its
    // walk passes 30 minutes, because a mode reaches within them; the
    // Gym row shows "-" where a mode has no time; Martial arts, with no
    // place within 30 minutes by any mode, takes the "none within" line.
    let rows = vec![
        EvaluationRow {
            place_type: "Work".to_owned(),
            destination_location_id: 11,
            destination: "Alex office".to_owned(),
            walk: Some(45),
            bike: Some(18),
            transit: Some(22),
            car: Some(15),
        },
        EvaluationRow {
            place_type: "School".to_owned(),
            destination_location_id: 12,
            destination: "Grundschule Neumark".to_owned(),
            walk: Some(12),
            bike: Some(5),
            transit: Some(14),
            car: Some(7),
        },
        EvaluationRow {
            place_type: "Gym".to_owned(),
            destination_location_id: 13,
            destination: "Urban Apes".to_owned(),
            walk: Some(8),
            bike: Some(4),
            transit: None,
            car: Some(6),
        },
    ];
    let expected = [
        "Inselstraße 10, Mitte        walk  bike  transit  car",
        "Work: Alex office              45    18       22   15",
        "School: Grundschule Neumark    12     5       14    7",
        "Gym: Urban Apes                 8     4        -    6",
        "Martial arts: none within 30 min by any mode",
    ]
    .join("\n");

    assert_eq!(
        render(
            "Inselstraße 10, Mitte",
            &["Work", "School", "Gym", "Martial arts"],
            &rows,
        ),
        expected
    );
}
