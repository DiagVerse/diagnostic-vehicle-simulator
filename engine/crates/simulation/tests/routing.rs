//! Routing tests for the simulation service: load a CAN log, then drive the reconstructed
//! ECUs by CAN address through the real UDS protocol logic.
//!
//! The UDS plugin is linked as an rlib rather than loaded as a dynamic library, so these
//! tests exercise the same protocol implementation the engine loads at runtime without
//! needing a built plugin on disk.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{RoutingOutcome, SimulationService};

const c_strSingleEcuLog: &str = include_str!("fixtures/single_ecu.log");
const c_strTwoEcuLog: &str = include_str!("fixtures/two_ecus.log");

/// Bridges the UDS plugin's pure handler to the engine's `ProtocolHandler` port.
struct UdsHandler;

impl ProtocolHandler for UdsHandler {
    fn Handle(&self, vecRequest: RVec<u8>, snapshot: REcuSnapshot) -> RProtocolOutcome {
        let reply = uds_plugin::handler::HandleRequest(vecRequest.as_slice(), &snapshot);
        RProtocolOutcome {
            m_vecResponse: RVec::from(reply.m_vecResponse),
            m_vecChanges: RVec::from(reply.m_vecChanges),
        }
    }

    fn Name(&self) -> &str {
        "uds"
    }
}

/// Send one request and assert it was answered by exactly one ECU; return that answer.
fn SendExpectingOneAnswer(
    simulation: &mut SimulationService,
    u32RequestCanId: u32,
    vecRequest: &[u8],
) -> (u32, Vec<u8>) {
    let outcome = simulation.ProcessByCanId(u32RequestCanId, vecRequest, &UdsHandler);
    match outcome {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(vecResponses.len(), 1, "exactly one ECU should answer");
            let response = &vecResponses[0];
            (response.m_u32ResponseCanId, response.m_vecResponse.clone())
        }
        RoutingOutcome::NoTarget => {
            panic!("expected an ECU on CAN id 0x{u32RequestCanId:03X}, but none was routed to")
        }
    }
}

#[test]
fn routes_a_request_to_the_ecu_that_owns_the_request_can_id() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    let (u32ResponseCanId, vecResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);

    assert_eq!(u32ResponseCanId, 0x7E8);
    assert_eq!(&vecResponse[0..3], &[0x62, 0xF1, 0x90]);
    assert_eq!(&vecResponse[3..], b"VIN0123456789ABCD");
}

#[test]
fn an_unknown_request_can_id_gets_no_response_at_all() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    // No ECU listens on 0x7E5. A real bus is silent here — it does not answer with an NRC.
    let outcome = simulation.ProcessByCanId(0x7E5, &[0x22, 0xF1, 0x90], &UdsHandler);
    assert_eq!(outcome, RoutingOutcome::NoTarget);
}

#[test]
fn each_ecu_of_a_two_ecu_log_answers_on_its_own_identifier() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strTwoEcuLog)
        .expect("the two-ECU log should load");

    assert_eq!(simulation.RunningEcus().count(), 2);

    let (u32FirstResponseId, vecFirstResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);
    assert_eq!(u32FirstResponseId, 0x7E8);
    assert_eq!(&vecFirstResponse[3..], b"EAA");

    let (u32SecondResponseId, vecSecondResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x7E1, &[0x22, 0xF1, 0x90]);
    assert_eq!(u32SecondResponseId, 0x7E9);
    assert_eq!(&vecSecondResponse[3..], b"TCM");
}

#[test]
fn session_state_is_kept_per_ecu() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strTwoEcuLog)
        .expect("the two-ECU log should load");

    let (_, vecResponse) = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x03]);
    assert_eq!(&vecResponse[0..2], &[0x50, 0x03]);

    // Only the addressed ECU changed session; the other is still in the default session.
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .expect("ECU on 0x7E0")
            .CurrentSession(),
        0x03
    );
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E1)
            .expect("ECU on 0x7E1")
            .CurrentSession(),
        0x01
    );
}

#[test]
fn an_unknown_did_is_answered_with_request_out_of_range() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    // 0xF1FF was never observed in the log, so the ECU does not hold it.
    let (u32ResponseCanId, vecResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0xFF]);

    assert_eq!(u32ResponseCanId, 0x7E8);
    assert_eq!(vecResponse, vec![0x7F, 0x22, 0x31]);
}

#[test]
fn a_service_the_ecu_never_showed_is_answered_with_service_not_supported() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    // SecurityAccess never appears in the log, so the reconstructed ECU does not support it.
    let (_, vecResponse) = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x27, 0x01]);
    assert_eq!(vecResponse, vec![0x7F, 0x27, 0x11]);
}

#[test]
fn a_suppressed_positive_response_is_routed_but_carries_no_bytes() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    // TesterPresent with the suppressPosRspMsgIndicationBit set: handled, but nothing is sent.
    let outcome = simulation.ProcessByCanId(0x7E0, &[0x3E, 0x80], &UdsHandler);
    match outcome {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(vecResponses.len(), 1);
            assert!(vecResponses[0].IsSuppressed());
        }
        RoutingOutcome::NoTarget => panic!("the ECU on 0x7E0 should have handled the request"),
    }
}

#[test]
fn resetting_returns_every_ecu_to_the_default_session() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strTwoEcuLog)
        .expect("the two-ECU log should load");

    SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x03]);
    SendExpectingOneAnswer(&mut simulation, 0x7E1, &[0x10, 0x03]);

    simulation.ResetAllEcus();

    for (_, runningEcu) in simulation.RunningEcus() {
        assert_eq!(runningEcu.CurrentSession(), 0x01);
    }
}

#[test]
fn a_log_that_cannot_be_reconstructed_leaves_the_loaded_simulation_untouched() {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strSingleEcuLog)
        .expect("the golden log should load");

    let resError = simulation.LoadFromLogText("this is not a CAN log at all");
    assert!(resError.is_err(), "an unparseable log must be rejected");

    // The working simulation survived the bad upload.
    let (u32ResponseCanId, _) = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);
    assert_eq!(u32ResponseCanId, 0x7E8);
}
