//! Timing behaviour of the simulated ECUs: response delays, NRC 0x78 ResponsePending
//! sequences, and the P2/P2* values advertised in the DiagnosticSessionControl response.
//!
//! These assert on the **plan** — the byte strings and their millisecond offsets — so no test
//! here sleeps. Executing a plan against a real clock is the transport's job and is tested
//! separately.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::EcuTiming;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{RoutedResponse, RoutingOutcome, SimulationService};

/// Three ECUs: 0x7E0/0x7E8 (services 0x10, 0x22, 0x3E; DID 0xF190), the OEM pair 0x745/0x765
/// (services 0x10, 0x3E), and the 29-bit 0x18DAD4F1/0x18DAF1D4 (services 0x10, 0x22; DID
/// 0xF190). See the fixture.
const c_strThreeEcuLog: &str = include_str!("fixtures/three_ecus.log");

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

fn LoadSimulation() -> SimulationService {
    let mut simulation = SimulationService::New();
    simulation
        .LoadFromLogText(c_strThreeEcuLog)
        .expect("the three-ECU log should load");
    simulation
}

/// Timing that forces `u8Count` ResponsePending messages ahead of a delayed answer.
fn ForcedPendingTiming(u32DelayMs: u32, u8Count: u8) -> EcuTiming {
    EcuTiming {
        m_u32ResponseDelayMs: u32DelayMs,
        m_bForceResponsePending: true,
        m_u8ForcedResponsePendingCount: u8Count,
        ..EcuTiming::default()
    }
}

fn SendExpectingOneAnswer(
    simulation: &mut SimulationService,
    u32RequestCanId: u32,
    vecRequest: &[u8],
) -> RoutedResponse {
    match simulation.ProcessByCanId(u32RequestCanId, vecRequest, &UdsHandler) {
        RoutingOutcome::Handled(vecResponses) => {
            assert_eq!(vecResponses.len(), 1, "exactly one ECU should answer");
            vecResponses.into_iter().next().expect("one answer")
        }
        outcome => panic!("CAN id 0x{u32RequestCanId:03X} should be routable, got {outcome:?}"),
    }
}

/// The offsets of a routed answer's scheduled messages.
fn OffsetsOf(response: &RoutedResponse) -> Vec<u32> {
    response
        .m_plan
        .m_vecSteps
        .iter()
        .map(|step| step.m_u32AtMs)
        .collect()
}

#[test]
fn a_delay_beyond_p2_produces_a_response_pending_before_the_answer() {
    let mut simulation = LoadSimulation();
    simulation
        .SetEcuTiming(
            0x7E0,
            EcuTiming {
                m_u32ResponseDelayMs: 200,
                ..EcuTiming::default()
            },
        )
        .expect("ECU on 0x7E0");

    let response = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);

    assert_eq!(OffsetsOf(&response), vec![50, 200]);
    assert_eq!(
        response.m_plan.m_vecSteps[0].m_vecBytes,
        vec![0x7F, 0x22, 0x78]
    );
    assert_eq!(&response.m_vecResponse[0..3], &[0x62, 0xF1, 0x90]);
    assert!(response.m_plan.m_bIsIsoConformant);
}

#[test]
fn a_response_pending_overrides_a_suppressed_positive_response() {
    let mut simulation = LoadSimulation();

    // Control: with default timing, TesterPresent with the suppressPosRspMsgIndicationBit set
    // sends nothing at all.
    let suppressed = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x3E, 0x80]);
    assert!(suppressed.IsSuppressed());
    assert!(suppressed.m_plan.m_vecSteps.is_empty());

    // Once a ResponsePending is in play the server must send a final response regardless of
    // that bit (ISO 14229-1 Annex A.1, and the third condition of the clause 7.5.5 pseudocode).
    simulation
        .SetEcuTiming(0x7E0, ForcedPendingTiming(200, 1))
        .expect("ECU on 0x7E0");

    let answered = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x3E, 0x80]);

    assert!(!answered.IsSuppressed());
    assert_eq!(answered.m_vecResponse, vec![0x7E, 0x00]);
    assert_eq!(OffsetsOf(&answered), vec![50, 200]);
    assert_eq!(
        answered.m_plan.m_vecSteps[0].m_vecBytes,
        vec![0x7F, 0x3E, 0x78]
    );
}

#[test]
fn a_suppressed_session_change_still_changes_session_and_answers_after_a_pending() {
    let mut simulation = LoadSimulation();
    simulation
        .SetEcuTiming(0x7E0, ForcedPendingTiming(300, 2))
        .expect("ECU on 0x7E0");

    let response = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x83]);

    assert_eq!(OffsetsOf(&response), vec![50, 175, 300]);
    // The final response is the full six-byte positive response, not a suppressed nothing.
    assert_eq!(
        response.m_vecResponse,
        vec![0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]
    );
    assert_eq!(response.m_bySession, 0x03);
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .expect("ECU on 0x7E0")
            .CurrentSession(),
        0x03
    );
}

#[test]
fn the_session_response_advertises_the_ecus_own_p2_and_p2_star() {
    let mut simulation = LoadSimulation();

    // Defaults: P2 = 50 ms -> 0x0032, P2* = 5000 ms / 10 ms units = 500 -> 0x01F4
    // (ISO 14229-1 Table 29).
    let before = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x03]);
    assert_eq!(
        before.m_vecResponse,
        vec![0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]
    );

    simulation
        .SetEcuTiming(
            0x7E0,
            EcuTiming {
                m_u32P2ServerMaxMs: 100,
                m_u32P2StarServerMaxMs: 10_000,
                ..EcuTiming::default()
            },
        )
        .expect("ECU on 0x7E0");

    // A read in between must not announce anything: ISO 14229-1 carries these values only in
    // the DiagnosticSessionControl response.
    let read = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);
    assert_eq!(&read.m_vecResponse[0..3], &[0x62, 0xF1, 0x90]);

    // P2 = 100 ms -> 0x0064, P2* = 10000 / 10 = 1000 -> 0x03E8.
    let after = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x03]);
    assert_eq!(
        after.m_vecResponse,
        vec![0x50, 0x03, 0x00, 0x64, 0x03, 0xE8]
    );
}

#[test]
fn an_unsupported_service_never_draws_a_response_pending() {
    let mut simulation = LoadSimulation();
    simulation
        .SetEcuTiming(0x7E0, ForcedPendingTiming(200, 1))
        .expect("ECU on 0x7E0");

    // 0x28 CommunicationControl was never observed, so the ECU does not support it.
    // ISO 14229-2 clause 7.1.1: an unsupported service has P4Server_max == P2Server_max, which
    // forbids NRC 0x78 — the refusal must be immediate.
    let response = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x28, 0x00]);

    assert_eq!(response.m_plan.m_u8ResponsePendingCount, 0);
    assert_eq!(response.m_vecResponse, vec![0x7F, 0x28, 0x11]);
    assert_eq!(OffsetsOf(&response), vec![200]);
}

#[test]
fn a_pending_on_a_broadcast_un_suppresses_the_final_negative_response() {
    let mut simulation = LoadSimulation();

    // Control: the 29-bit ECU supports 0x22 but has no DID 0xF186, so it answers NRC 0x31
    // requestOutOfRange — which ISO 14229-1 clause 7.5.1 suppresses on a broadcast.
    let outcome = simulation.ProcessByCanId(0x18DB33F1, &[0x22, 0xF1, 0x86], &UdsHandler);
    assert_eq!(outcome, RoutingOutcome::Handled(Vec::new()));

    simulation
        .SetEcuTiming(0x18DAD4F1, ForcedPendingTiming(200, 1))
        .expect("ECU on 0x18DAD4F1");

    // Having announced itself with a ResponsePending, the server must now send the final
    // negative response too (ISO 14229-1 clause 7.5.5 and Annex A.1) — going quiet would
    // strand the tester until P2* expires.
    let response = SendExpectingOneAnswer(&mut simulation, 0x18DB33F1, &[0x22, 0xF1, 0x86]);

    assert_eq!(response.m_u32ResponseCanId, 0x18DAF1D4);
    assert_eq!(response.m_plan.m_u8ResponsePendingCount, 1);
    assert_eq!(response.m_vecResponse, vec![0x7F, 0x22, 0x31]);
    assert_eq!(OffsetsOf(&response), vec![50, 200]);
}

#[test]
fn a_dropped_final_response_leaves_the_tester_waiting_after_the_pending() {
    let mut simulation = LoadSimulation();
    simulation
        .SetEcuTiming(
            0x7E0,
            EcuTiming {
                m_bDropFinalResponse: true,
                ..ForcedPendingTiming(200, 1)
            },
        )
        .expect("ECU on 0x7E0");

    let response = SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x22, 0xF1, 0x90]);

    assert_eq!(OffsetsOf(&response), vec![50]);
    assert!(response.m_plan.m_bIsFinalResponseDropped);
    assert!(response.IsSuppressed());
    // A server that never finishes is not conformant, and the engine says so rather than
    // presenting the silence as normal.
    assert!(!response.m_plan.m_bIsIsoConformant);
}

#[test]
fn timing_survives_a_reset_but_diagnostic_state_does_not() {
    let mut simulation = LoadSimulation();
    simulation
        .SetEcuTiming(0x7E0, ForcedPendingTiming(200, 1))
        .expect("ECU on 0x7E0");
    SendExpectingOneAnswer(&mut simulation, 0x7E0, &[0x10, 0x03]);

    simulation.ResetAllEcus();

    // The session is back to default; the operator's fault configuration is untouched.
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .expect("ECU on 0x7E0")
            .CurrentSession(),
        0x01
    );
    let timing = simulation.EcuTimingOf(0x7E0).expect("ECU on 0x7E0");
    assert_eq!(timing.m_u32ResponseDelayMs, 200);
    assert!(timing.m_bForceResponsePending);
}

#[test]
fn setting_timing_on_an_unknown_identifier_is_refused() {
    let mut simulation = LoadSimulation();
    let resError = simulation.SetEcuTiming(0x7E5, EcuTiming::default());
    assert!(resError.is_err());
}
