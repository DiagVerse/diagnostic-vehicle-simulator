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
/// Three ECUs covering every addressing shape the MVP simulates: a legislated 11-bit pair, an
/// OEM 11-bit pair, and a 29-bit normal-fixed pair. See the fixture for the identifier map.
const c_strThreeEcuLog: &str = include_str!("fixtures/three_ecus.log");

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

/// Load the three-ECU fixture into a fresh simulation.
fn LoadThreeEcuSimulation() -> SimulationService {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strThreeEcuLog)
        .expect("the three-ECU log should load");
    simulation
}

/// Send one request and assert the identifier was known; return every answer it produced.
fn SendExpectingHandled(
    simulation: &mut SimulationService,
    u32RequestCanId: u32,
    vecRequest: &[u8],
) -> Vec<simulation::RoutedResponse> {
    match simulation.ProcessByCanId(u32RequestCanId, vecRequest, &UdsHandler) {
        RoutingOutcome::Handled(vecResponses) => vecResponses,
        RoutingOutcome::NoTarget => {
            panic!("CAN id 0x{u32RequestCanId:03X} should be a known identifier")
        }
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

// ---------------------------------------------------------------------------------------
// Functional (broadcast) addressing.
// ---------------------------------------------------------------------------------------

#[test]
fn a_broadcast_is_answered_by_every_listening_ecu_in_arbitration_order() {
    let mut simulation = LoadThreeEcuSimulation();

    let vecResponses = SendExpectingHandled(&mut simulation, 0x7DF, &[0x3E, 0x00]);

    // Both 11-bit ECUs listen on 0x7DF; the 29-bit ECU listens on 0x18DB33F1 and must not
    // answer a request from another network's broadcast identifier.
    assert_eq!(vecResponses.len(), 2);
    // CAN arbitration is won by the lower identifier, so 0x765 is on the bus first.
    assert_eq!(vecResponses[0].m_u32ResponseCanId, 0x765);
    assert_eq!(vecResponses[1].m_u32ResponseCanId, 0x7E8);
    for response in &vecResponses {
        assert_eq!(response.m_vecResponse, vec![0x7E, 0x00]);
    }
}

#[test]
fn a_broadcast_only_draws_answers_from_ecus_that_support_the_service() {
    let mut simulation = LoadThreeEcuSimulation();

    let vecResponses = SendExpectingHandled(&mut simulation, 0x7DF, &[0x22, 0xF1, 0x90]);

    // ECU_765 never showed service 0x22, so it would answer NRC 0x11 serviceNotSupported —
    // which ISO 14229-1 requires a functionally addressed server to suppress.
    assert_eq!(vecResponses.len(), 1);
    assert_eq!(vecResponses[0].m_u32ResponseCanId, 0x7E8);
    assert_eq!(&vecResponses[0].m_vecResponse[0..3], &[0x62, 0xF1, 0x90]);
    assert_eq!(&vecResponses[0].m_vecResponse[3..], b"VIN0123456789ABCD");
}

#[test]
fn a_broadcast_for_an_unknown_did_draws_no_answer_at_all() {
    let mut simulation = LoadThreeEcuSimulation();

    // Physically this DID yields NRC 0x31 requestOutOfRange (asserted elsewhere). Functionally
    // both 0x31 and 0x11 are suppressed, so the bus stays silent — but the identifier was
    // known and the ECUs did process the request, which is not the same as NoTarget.
    let vecResponses = SendExpectingHandled(&mut simulation, 0x7DF, &[0x22, 0x12, 0x34]);
    assert!(vecResponses.is_empty());
}

#[test]
fn an_unsupported_session_is_refused_physically_but_silent_functionally() {
    let mut simulation = LoadThreeEcuSimulation();

    // 0x04 safetySystemDiagnosticSession was never observed, so no ECU supports it.
    let (_, vecResponse) = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x04]);
    assert_eq!(vecResponse, vec![0x7F, 0x10, 0x12]);

    // NRC 0x12 sub-functionNotSupported is suppressed on a functionally addressed request.
    let vecResponses = SendExpectingHandled(&mut simulation, 0x7DF, &[0x10, 0x04]);
    assert!(vecResponses.is_empty());
}

#[test]
fn a_broadcast_that_suppresses_positive_responses_still_changes_state() {
    let mut simulation = LoadThreeEcuSimulation();

    // suppressPosRspMsgIndicationBit set on DiagnosticSessionControl(extended).
    let vecResponses = SendExpectingHandled(&mut simulation, 0x7DF, &[0x10, 0x83]);

    assert_eq!(
        vecResponses.len(),
        2,
        "both listeners processed the request"
    );
    for response in &vecResponses {
        assert!(response.IsSuppressed(), "nothing goes on the wire");
        // Suppression hides the response, never the state change.
        assert_eq!(response.m_bySession, 0x03);
    }
}

#[test]
fn a_broadcast_too_long_for_a_single_frame_is_ignored() {
    let mut simulation = LoadThreeEcuSimulation();

    // A broadcast has no single peer to send flow control, so it cannot be segmented.
    let vecRequest = [0x22, 0xF1, 0x90, 0xF1, 0x91, 0xF1, 0x92, 0xF1];
    let outcome = simulation.ProcessByCanId(0x7DF, &vecRequest, &UdsHandler);
    assert_eq!(outcome, RoutingOutcome::NoTarget);
}

#[test]
fn the_29_bit_broadcast_only_reaches_the_29_bit_ecu() {
    let mut simulation = LoadThreeEcuSimulation();

    let vecResponses = SendExpectingHandled(&mut simulation, 0x18DB33F1, &[0x10, 0x03]);

    assert_eq!(vecResponses.len(), 1);
    assert_eq!(vecResponses[0].m_u32ResponseCanId, 0x18DAF1D4);
    assert_eq!(&vecResponses[0].m_vecResponse[0..2], &[0x50, 0x03]);
}

// ---------------------------------------------------------------------------------------
// 29-bit normal fixed addressing.
// ---------------------------------------------------------------------------------------

#[test]
fn a_29_bit_request_is_answered_with_the_target_and_source_swapped() {
    let mut simulation = LoadThreeEcuSimulation();

    let (u32ResponseCanId, vecResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x18DAD4F1, &[0x22, 0xF1, 0x90]);

    assert_eq!(u32ResponseCanId, 0x18DAF1D4);
    assert_eq!(&vecResponse[3..], b"JN8AY3NY5T9100001");
}

#[test]
fn a_29_bit_identifier_with_no_ecu_behind_it_gets_no_response() {
    let mut simulation = LoadThreeEcuSimulation();

    // 0x18DADAF1 is a well-formed normal-fixed identifier for target 0xDA — an ECU that is
    // not on this vehicle. Routing must key on the exact identifier, not on the pattern.
    let outcome = simulation.ProcessByCanId(0x18DADAF1, &[0x10, 0x01], &UdsHandler);
    assert_eq!(outcome, RoutingOutcome::NoTarget);
}

#[test]
fn a_physical_request_changes_only_the_addressed_ecus_session() {
    let mut simulation = LoadThreeEcuSimulation();

    // The OEM pair: the answer comes back on 0x765, not on 0x74D.
    let (u32ResponseCanId, vecResponse) =
        SendExpectingOneAnswer(&mut simulation, 0x745, &[0x10, 0x03]);
    assert_eq!(u32ResponseCanId, 0x765);
    assert_eq!(&vecResponse[0..2], &[0x50, 0x03]);

    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x745)
            .expect("ECU on 0x745")
            .CurrentSession(),
        0x03
    );
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .expect("ECU on 0x7E0")
            .CurrentSession(),
        0x01
    );
}
