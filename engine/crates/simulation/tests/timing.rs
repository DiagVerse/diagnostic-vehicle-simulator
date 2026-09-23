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
use simulation::{EcuKey, RoutedResponse, RoutingOutcome, SimulationService};

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
            EcuKey::Can(0x7E0),
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
        .SetEcuTiming(EcuKey::Can(0x7E0), ForcedPendingTiming(200, 1))
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
        .SetEcuTiming(EcuKey::Can(0x7E0), ForcedPendingTiming(300, 2))
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
            EcuKey::Can(0x7E0),
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
        .SetEcuTiming(EcuKey::Can(0x7E0), ForcedPendingTiming(200, 1))
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
        .SetEcuTiming(EcuKey::Can(0x18DAD4F1), ForcedPendingTiming(200, 1))
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
            EcuKey::Can(0x7E0),
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
        .SetEcuTiming(EcuKey::Can(0x7E0), ForcedPendingTiming(200, 1))
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
    let timing = simulation
        .EcuTimingOf(EcuKey::Can(0x7E0))
        .expect("ECU on 0x7E0");
    assert_eq!(timing.m_u32ResponseDelayMs, 200);
    assert!(timing.m_bForceResponsePending);
}

#[test]
fn setting_timing_on_an_unknown_identifier_is_refused() {
    let mut simulation = LoadSimulation();
    let resError = simulation.SetEcuTiming(EcuKey::Can(0x7E5), EcuTiming::default());
    assert!(resError.is_err());
}

// ---------------------------------------------------------------- bulk flow control

#[test]
fn flow_control_applies_to_every_ecu_at_once() {
    // The chore this exists to relieve: the link a BlockSize compensates for is shared by the
    // whole vehicle, so the value is the same for all of them — and setting it forty-seven
    // times by hand is a cost the per-ECU model imposes without buying anything.
    let mut simulation = LoadSimulation();

    let vecChanged = simulation
        .SetFlowControlForEveryEcu(4, 0x0A)
        .expect("BlockSize 4 and STmin 10 ms are both legal");

    let uEcus = simulation.RunningEcus().count();
    assert_eq!(vecChanged.len(), uEcus, "every ECU, not just the first");

    let vecKeys: Vec<EcuKey> = simulation.RunningEcus().map(|(key, _)| key).collect();
    for key in vecKeys {
        let timing = simulation.EcuTimingOf(key).expect("the ECU exists");
        assert_eq!(timing.m_u8IsoTpBlockSize, 4);
        assert_eq!(timing.m_byIsoTpSeparationTimeMin, 0x0A);
    }
}

#[test]
fn a_bulk_apply_does_not_overwrite_deliberate_per_ecu_timing() {
    // The property that makes this action safe to offer at all. An operator sets a response
    // delay and a forced ResponsePending on *one* ECU on purpose — that is fault injection,
    // often the whole point of the session. A bulk action that silently undid it while fixing
    // an unrelated link problem would be worse than no bulk action.
    let mut simulation = LoadSimulation();
    let key = EcuKey::Can(0x7E0);

    let injected = ForcedPendingTiming(250, 3);
    simulation
        .SetEcuTiming(key, injected)
        .expect("the ECU exists");

    simulation
        .SetFlowControlForEveryEcu(8, 0x14)
        .expect("legal values");

    let after = simulation.EcuTimingOf(key).expect("the ECU exists");
    assert_eq!(after.m_u8IsoTpBlockSize, 8, "flow control did change");
    assert_eq!(after.m_byIsoTpSeparationTimeMin, 0x14);

    assert_eq!(
        after.m_u32ResponseDelayMs, 250,
        "the injected delay survived"
    );
    assert!(after.m_bForceResponsePending, "and so did the forced 0x78");
    assert_eq!(after.m_u8ForcedResponsePendingCount, 3);
    assert_eq!(
        after.m_u32P2ServerMaxMs, injected.m_u32P2ServerMaxMs,
        "P2 was never this action's business"
    );
}

#[test]
fn applying_the_same_flow_control_twice_reports_nothing_to_do() {
    // "47 ECUs changed" after a no-op would read as work having happened, and would send
    // someone looking for what moved.
    let mut simulation = LoadSimulation();

    let vecFirst = simulation
        .SetFlowControlForEveryEcu(2, 0x00)
        .expect("legal values");
    assert!(!vecFirst.is_empty(), "the first apply changes things");

    let vecSecond = simulation
        .SetFlowControlForEveryEcu(2, 0x00)
        .expect("legal values");
    assert!(
        vecSecond.is_empty(),
        "the second changes nothing, and says so: {vecSecond:?}"
    );
}

#[test]
fn a_reserved_separation_time_is_refused_before_anything_is_changed() {
    // 0x80-0xF0 is reserved by ISO 15765-2. Validating up front rather than per ECU is what
    // keeps a refusal from leaving half a vehicle on the new value and half on the old — a
    // state no operator asked for and none would think to check.
    let mut simulation = LoadSimulation();
    let key = EcuKey::Can(0x7E0);
    let before = simulation.EcuTimingOf(key).expect("the ECU exists");

    let result = simulation.SetFlowControlForEveryEcu(1, 0x90);
    assert!(result.is_err(), "0x90 is reserved, not a separation time");

    let after = simulation.EcuTimingOf(key).expect("the ECU exists");
    assert_eq!(
        after.m_byIsoTpSeparationTimeMin, before.m_byIsoTpSeparationTimeMin,
        "and nothing was changed on the way to refusing"
    );
}

#[test]
fn a_running_bridge_is_told_the_vehicle_changed() {
    // BlockSize and STmin are cached in a live bridge's receivers. Without the generation bump
    // the operator's change would not reach the wire until the link was restarted — which is
    // exactly the class of "I fixed it and nothing happened" this project has hit before.
    let mut simulation = LoadSimulation();
    let u64Before = simulation.ConfigGeneration();

    simulation
        .SetFlowControlForEveryEcu(6, 0x05)
        .expect("legal values");

    let u64AfterChange = simulation.ConfigGeneration();
    assert_ne!(
        u64AfterChange, u64Before,
        "the bridge rebuilds its receivers on this"
    );

    // And the other half: an apply that changes nothing must not rebuild anything either.
    // A live bridge dropping and re-making every receiver because someone pressed a button
    // twice is a stall on the wire with no cause an operator could name.
    simulation
        .SetFlowControlForEveryEcu(6, 0x05)
        .expect("legal values");
    assert_eq!(
        simulation.ConfigGeneration(),
        u64AfterChange,
        "a no-op apply leaves a running link alone"
    );
}
