//! The TCP_DATA connection state machine, and the routing activation decision table.
//!
//! This is where ISO 13400-2 is at its most intricate, and where several of the listed
//! conformance traps live. Each test names the rule it pins.

#![allow(non_snake_case, non_upper_case_globals)]

use doip::connection::{Connection, ConnectionState};
use doip::messages::{RoutingActivationOutcome, RoutingActivationRequest};

const c_u16Tester: u16 = 0x0E80;
const c_u16OtherTester: u16 = 0x0E81;

fn Request(u16SourceAddress: u16, byActivationType: u8) -> RoutingActivationRequest {
    RoutingActivationRequest {
        m_u16SourceAddress: u16SourceAddress,
        m_byActivationType: byActivationType,
        m_u32ReservedIso: 0,
        m_optU32ReservedOem: None,
    }
}

/// A connection that has successfully activated.
fn Activated() -> Connection {
    let mut connection = Connection::New();
    let request = Request(c_u16Tester, 0x00);
    let outcome = connection.DecideRoutingActivation(&request, true, false, true);
    connection.ApplyRoutingActivation(&request, outcome);
    connection
}

#[test]
fn a_fresh_connection_routes_nothing() {
    // REQ 3.DoIP-131 NL: nothing is answered or routed before routing is active.
    let connection = Connection::New();
    assert_eq!(connection.State(), ConnectionState::Initialized);
    assert!(!connection.IsRoutingActive());
    assert_eq!(connection.SourceAddress(), None);
}

#[test]
fn a_valid_request_activates_and_registers_the_address() {
    let connection = Activated();
    assert_eq!(connection.State(), ConnectionState::RoutingActive);
    assert!(connection.IsRoutingActive());
    assert_eq!(connection.SourceAddress(), Some(c_u16Tester));
}

#[test]
fn re_activating_the_same_socket_with_the_same_address_is_accepted() {
    // REQ 3.DoIP-089 NL, and a listed trap: rejecting this is a common bug. Only a *different*
    // address on an already-activated socket is an error.
    let connection = Activated();
    let outcome =
        connection.DecideRoutingActivation(&Request(c_u16Tester, 0x00), true, false, true);
    assert_eq!(outcome, RoutingActivationOutcome::Activated);
}

#[test]
fn activating_the_same_socket_with_a_different_address_is_refused_and_closes_it() {
    // REQ 3.DoIP-106 / 3.DoIP-149 NL → response code 0x02.
    let mut connection = Activated();
    let request = Request(c_u16OtherTester, 0x00);

    let outcome = connection.DecideRoutingActivation(&request, true, false, true);
    assert_eq!(
        outcome,
        RoutingActivationOutcome::DeniedSourceAddressMismatch
    );
    assert_eq!(outcome.Code(), 0x02);
    assert!(outcome.ClosesSocket());

    connection.ApplyRoutingActivation(&request, outcome);
    assert_eq!(connection.State(), ConnectionState::Finalize);
}

#[test]
fn an_address_already_live_on_another_socket_is_refused() {
    let connection = Connection::New();
    let outcome = connection.DecideRoutingActivation(&Request(c_u16Tester, 0x00), true, true, true);
    assert_eq!(outcome.Code(), 0x03);
    assert!(outcome.ClosesSocket());
}

#[test]
fn no_free_socket_is_refused_with_its_own_code() {
    let connection = Connection::New();
    let outcome =
        connection.DecideRoutingActivation(&Request(c_u16Tester, 0x00), true, false, false);
    assert_eq!(outcome.Code(), 0x01);
}

#[test]
fn an_unacceptable_source_address_is_refused() {
    let connection = Connection::New();
    let outcome = connection.DecideRoutingActivation(&Request(0x1234, 0x00), false, false, true);
    assert_eq!(outcome.Code(), 0x00);
}

#[test]
fn a_reserved_activation_type_is_refused_rather_than_silently_accepted() {
    // 0x02-0xDF are ISO-reserved; answering 0x10 to one would tell a tester something is set up
    // when nothing is.
    let connection = Connection::New();
    for byActivationType in [0x02u8, 0x7F, 0xDF, 0xE0] {
        let outcome = connection.DecideRoutingActivation(
            &Request(c_u16Tester, byActivationType),
            true,
            false,
            true,
        );
        assert_eq!(
            outcome,
            RoutingActivationOutcome::DeniedUnsupportedActivationType,
            "activation type 0x{byActivationType:02X} is not one we implement"
        );
    }
}

#[test]
fn the_two_mandatory_activation_types_are_supported() {
    let connection = Connection::New();
    for byActivationType in [0x00u8, 0x01] {
        let outcome = connection.DecideRoutingActivation(
            &Request(c_u16Tester, byActivationType),
            true,
            false,
            true,
        );
        assert_eq!(outcome, RoutingActivationOutcome::Activated);
    }
}

#[test]
fn an_unactivated_socket_dies_to_the_short_timer() {
    // T_TCP_Initial_Inactivity is 2 s — a measure against connections that send nothing, or
    // nothing valid.
    let mut connection = Connection::New();
    assert!(!connection.Tick(1_900), "still inside the two seconds");
    assert!(connection.Tick(200), "and now past them");
    assert_eq!(connection.State(), ConnectionState::Finalize);
}

#[test]
fn an_activated_socket_gets_the_long_timer_instead() {
    // T_TCP_General_Inactivity is 5 minutes. An activated socket that idles for three seconds
    // is perfectly healthy, and killing it on the initial deadline would be wrong.
    let mut connection = Activated();
    assert!(!connection.Tick(3_000));
    assert!(!connection.Tick(290_000));
    assert!(connection.Tick(10_000), "five minutes idle closes it");
}

#[test]
fn a_socket_awaiting_authentication_is_not_killed_by_the_two_second_timer() {
    // The listed trap: the initial timer stops on RECEIPT of a valid routing activation
    // request, not on transmission of a positive response. Stopping it on the response kills
    // sockets parked in Pending-for-Authentication at exactly two seconds.
    let mut connection = Connection::New();
    let request = Request(c_u16Tester, 0x00);

    connection.ApplyRoutingActivation(
        &request,
        RoutingActivationOutcome::DeniedMissingAuthentication,
    );
    assert_eq!(connection.State(), ConnectionState::PendingAuthentication);
    assert_eq!(connection.SourceAddress(), Some(c_u16Tester));

    assert!(
        !connection.Tick(5_000),
        "five seconds is fine while authentication is outstanding"
    );
    assert!(!connection.IsRoutingActive(), "but nothing is routed yet");
}

#[test]
fn traffic_in_either_direction_keeps_a_socket_alive() {
    // REQ 3.DoIP-080 NL says "received or sent". Forgetting the outbound half is a listed trap:
    // a busy connection whose traffic is all responses would close under it.
    let mut connection = Activated();

    for _ in 0..10 {
        assert!(!connection.Tick(290_000));
        connection.NoteActivity();
    }
    assert!(connection.IsRoutingActive());
}
