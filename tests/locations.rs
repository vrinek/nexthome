//! Locations over a real database: the state enum, the three create paths
//! (address, coordinates, Google Maps link), duplicate rejection, update
//! and delete with their job and evaluation side effects, the archive
//! guard, and listing with filters. No test calls a live provider.

mod common;

use std::str::FromStr;

use nexthome::locations::{LocationError, State};

#[test]
fn every_state_round_trips_through_its_stored_name() {
    let states = [
        State::Inbox,
        State::Rejected,
        State::Accepted,
        State::Contacted,
        State::Waitlisted,
        State::Booked,
    ];
    for state in states {
        let text = state.as_str();
        assert_eq!(
            State::from_str(text).ok(),
            Some(state),
            "{text} must parse back into the state it names"
        );
    }
}

#[test]
fn every_state_carries_the_marker_colour_the_rails_app_gave_it() {
    let colours = [
        (State::Inbox, "#1982c4"),
        (State::Rejected, "#ff595e"),
        (State::Accepted, "#8ac926"),
        (State::Contacted, "#ffca3a"),
        (State::Waitlisted, "#f77f00"),
        (State::Booked, "#6a4c93"),
    ];
    for (state, colour) in colours {
        assert_eq!(state.color(), colour);
    }
}

#[test]
fn parsing_refuses_a_name_no_state_has() {
    let error = State::from_str("bookmarked").unwrap_err();
    assert!(
        matches!(error, LocationError::UnknownState(ref text) if text == "bookmarked"),
        "expected an unknown-state error, got: {error}"
    );
    assert_eq!(error.to_string(), "bookmarked is not a location state");
}
