//! User-defined response overrides: answering a request the bundled protocol does not
//! implement, refusing one it does, and staying silent for one in particular.
//!
//! The governing rule under test is that an override changes **what the ECU says, not what it
//! does** — with one deliberate exception, covered below, where saying "I refused" while
//! having already changed session would make the ECU incoherent.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::{
    CanAddress, CanAddressingMode, DataIdentifier, EchoSpan, Ecu, EcuTiming, OverrideAction,
    ResponseOverride, SecurityLevel, SessionType,
};
use core_domain::Confidence;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use simulation::{RoutedResponse, RoutingOutcome, SimulationService};

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

fn Substitute(vecResponse: &[u8]) -> OverrideAction {
    OverrideAction::Substitute {
        m_vecResponse: vecResponse.to_vec(),
        m_vecEchoSpans: Vec::new(),
    }
}

/// One ECU on 0x7E0/0x7E8 with a VIN and security configured.
fn BuildEcu(vecOverrides: Vec<ResponseOverride>) -> Ecu {
    let mut config = Ecu::New("Engine", 0);
    config.m_optCanAddress = Some(CanAddress::NewSpecified(
        0x7E0,
        0x7E8,
        CanAddressingMode::Normal11Bit,
    ));
    config.m_vecSupportedServices = vec![0x10, 0x11, 0x19, 0x22, 0x27, 0x3E];
    config.m_vecSupportedSessions = vec![SessionType::Default, SessionType::Extended];
    config.m_mapDids.insert(
        0xF190,
        DataIdentifier {
            m_u16Id: 0xF190,
            m_vecValue: b"SIMULATORVIN00001".to_vec(),
            m_confidence: Confidence::Unknown,
        },
    );
    config.m_vecSecurityLevels.push(SecurityLevel {
        m_byRequestSeedSubFunction: 0x01,
        m_vecSeed: vec![0x11, 0x22, 0x33, 0x44],
        m_vecExpectedKey: vec![0xAA, 0xBB, 0xCC, 0xDD],
    });
    config.m_vecResponseOverrides = vecOverrides;
    config
}

fn LoadWith(vecOverrides: Vec<ResponseOverride>) -> SimulationService {
    let mut simulation = SimulationService::New();
    simulation.CreateEmptyVehicle("Bench");
    simulation.AddEcu(BuildEcu(vecOverrides)).expect("the ECU");
    simulation
}

fn Send(simulation: &mut SimulationService, vecRequest: &[u8]) -> RoutedResponse {
    match simulation.ProcessByCanId(0x7E0, vecRequest, &UdsHandler) {
        RoutingOutcome::Handled(vecResponses) => {
            vecResponses.into_iter().next().expect("one answer")
        }
        RoutingOutcome::NoTarget => panic!("0x7E0 should be routable"),
    }
}

#[test]
fn an_override_answers_a_service_the_protocol_does_not_implement() {
    // WriteDataByIdentifier is not implemented, so without an override every 2E gets
    // NRC 0x11 — which is the entire reason this feature exists.
    let mut plain = LoadWith(Vec::new());
    assert_eq!(
        Send(&mut plain, &[0x2E, 0xF1, 0x90, 0x01]).m_vecResponse,
        vec![0x7F, 0x2E, 0x11]
    );

    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x2E, 0xF1, 0x90, 0x01],
        Substitute(&[0x6E, 0xF1, 0x90]),
    )]);
    assert_eq!(
        Send(&mut simulation, &[0x2E, 0xF1, 0x90, 0x01]).m_vecResponse,
        vec![0x6E, 0xF1, 0x90]
    );
}

#[test]
fn a_wildcard_override_echoes_the_identifier_it_was_asked_for() {
    // `22 ** **` with an echo span: the response must carry the DID that was requested
    // (ISO 14229-1 clause 10.2), or a tester correlating on it rejects the answer.
    let overrideRule = ResponseOverride {
        m_vecRequestPattern: vec![0x22, 0x00, 0x00],
        m_vecRequestMask: vec![0xFF, 0x00, 0x00],
        m_bMatchTrailingBytes: false,
        m_action: OverrideAction::Substitute {
            m_vecResponse: vec![0x62, 0x00, 0x00, 0xDE, 0xAD],
            m_vecEchoSpans: vec![EchoSpan {
                m_uRequestOffset: 1,
                m_uLength: 2,
                m_uResponseOffset: 1,
            }],
        },
        m_bIsEnabled: true,
        m_bRespondEvenIfSuppressed: false,
        m_strNote: "every DID reads back DE AD".to_string(),
    };

    let mut simulation = LoadWith(vec![overrideRule]);

    assert_eq!(
        Send(&mut simulation, &[0x22, 0xF1, 0x8C]).m_vecResponse,
        vec![0x62, 0xF1, 0x8C, 0xDE, 0xAD]
    );
    assert_eq!(
        Send(&mut simulation, &[0x22, 0x01, 0x23]).m_vecResponse,
        vec![0x62, 0x01, 0x23, 0xDE, 0xAD]
    );
}

#[test]
fn the_most_specific_override_wins_regardless_of_order() {
    // A wildcard family plus one exact rule. The exact rule must win wherever it applies, and
    // the order they were added must not matter.
    let wildcard = ResponseOverride {
        m_vecRequestPattern: vec![0x22, 0x00, 0x00],
        m_vecRequestMask: vec![0xFF, 0x00, 0x00],
        m_bMatchTrailingBytes: false,
        m_action: Substitute(&[0x62, 0x00, 0x00, 0x11]),
        m_bIsEnabled: true,
        m_bRespondEvenIfSuppressed: false,
        m_strNote: String::new(),
    };
    let exact =
        ResponseOverride::NewExact(&[0x22, 0xF1, 0x90], Substitute(&[0x62, 0xF1, 0x90, 0x99]));

    for vecOverrides in [
        vec![wildcard.clone(), exact.clone()],
        vec![exact.clone(), wildcard.clone()],
    ] {
        let mut simulation = LoadWith(vecOverrides);
        assert_eq!(
            Send(&mut simulation, &[0x22, 0xF1, 0x90]).m_vecResponse,
            vec![0x62, 0xF1, 0x90, 0x99],
            "the exact rule should win"
        );
        assert_eq!(
            Send(&mut simulation, &[0x22, 0xF1, 0x8C]).m_vecResponse[0],
            0x62,
            "the wildcard should still cover everything else"
        );
    }
}

#[test]
fn a_suppressing_override_makes_the_ecu_silent_for_one_request() {
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x22, 0xF1, 0x90],
        OverrideAction::Suppress,
    )]);

    // Silent for that one request...
    let silent = Send(&mut simulation, &[0x22, 0xF1, 0x90]);
    assert!(silent.IsSuppressed());
    assert!(silent.m_plan.m_vecSteps.is_empty());

    // ...and perfectly normal for everything else. "Present but silent for one request" is a
    // real failure that no negative response can express.
    assert_eq!(
        Send(&mut simulation, &[0x3E, 0x00]).m_vecResponse,
        vec![0x7E, 0x00]
    );
}

#[test]
fn an_override_changes_what_the_ecu_says_not_what_it_does() {
    // The VIN read is refused, but session behaviour is untouched.
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x22, 0xF1, 0x90],
        Substitute(&[0x7F, 0x22, 0x33]),
    )]);

    assert_eq!(
        Send(&mut simulation, &[0x10, 0x03]).m_vecResponse[0..2],
        [0x50, 0x03]
    );
    assert_eq!(
        Send(&mut simulation, &[0x22, 0xF1, 0x90]).m_vecResponse,
        vec![0x7F, 0x22, 0x33]
    );
    // The session change really happened, and the next request sees it.
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .unwrap()
            .CurrentSession(),
        0x03
    );
}

#[test]
fn refusing_a_session_change_rolls_the_session_back() {
    // The exception to the rule above: an ECU that says "I refused" while sitting in the
    // session it just entered is incoherent, and every later request would behave
    // inexplicably.
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x10, 0x03],
        Substitute(&[0x7F, 0x10, 0x22]),
    )]);

    let response = Send(&mut simulation, &[0x10, 0x03]);
    assert_eq!(response.m_vecResponse, vec![0x7F, 0x10, 0x22]);
    assert_eq!(response.m_bySession, 0x01, "still in the default session");
    assert_eq!(
        simulation
            .FindEcuByRequestCanId(0x7E0)
            .unwrap()
            .CurrentSession(),
        0x01
    );
}

#[test]
fn an_override_does_not_answer_a_request_the_tester_asked_not_to_be_answered() {
    // `3E 80` sets the suppressPosRspMsgIndicationBit: the tester is not listening, so putting
    // bytes on the wire would be fault injection rather than a fix.
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x3E, 0x80],
        Substitute(&[0x7E, 0x00]),
    )]);
    assert!(Send(&mut simulation, &[0x3E, 0x80]).IsSuppressed());

    // Unless it is asked for explicitly.
    let mut forced = LoadWith(vec![ResponseOverride {
        m_bRespondEvenIfSuppressed: true,
        ..ResponseOverride::NewExact(&[0x3E, 0x80], Substitute(&[0x7E, 0x00]))
    }]);
    assert_eq!(
        Send(&mut forced, &[0x3E, 0x80]).m_vecResponse,
        vec![0x7E, 0x00]
    );
}

#[test]
fn an_override_replaces_the_final_response_and_never_the_pending_messages() {
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x22, 0xF1, 0x90],
        Substitute(&[0x62, 0xF1, 0x90, 0x01, 0x02]),
    )]);
    simulation
        .SetEcuTiming(
            0x7E0,
            EcuTiming {
                m_u32ResponseDelayMs: 200,
                ..EcuTiming::default()
            },
        )
        .expect("ECU on 0x7E0");

    let response = Send(&mut simulation, &[0x22, 0xF1, 0x90]);

    assert_eq!(response.m_plan.m_vecSteps.len(), 2);
    // The ResponsePending bytes belong to the timing layer, not the operator.
    assert_eq!(
        response.m_plan.m_vecSteps[0].m_vecBytes,
        vec![0x7F, 0x22, 0x78]
    );
    assert_eq!(
        response.m_plan.m_vecSteps[1].m_vecBytes,
        vec![0x62, 0xF1, 0x90, 0x01, 0x02]
    );
}

#[test]
fn suppressing_after_a_pending_is_flagged_as_leaving_the_tester_waiting() {
    let mut simulation = LoadWith(vec![ResponseOverride::NewExact(
        &[0x22, 0xF1, 0x90],
        OverrideAction::Suppress,
    )]);
    simulation
        .SetEcuTiming(
            0x7E0,
            EcuTiming {
                m_u32ResponseDelayMs: 200,
                ..EcuTiming::default()
            },
        )
        .expect("ECU on 0x7E0");

    let response = Send(&mut simulation, &[0x22, 0xF1, 0x90]);

    // A ResponsePending is a promise to answer; going silent afterwards breaks it, and the
    // engine says so rather than presenting the silence as normal.
    assert_eq!(response.m_plan.m_u8ResponsePendingCount, 1);
    assert!(response.m_plan.FinalAtMs().is_none());
    assert!(!response.m_plan.m_bIsIsoConformant);
    assert!(response
        .m_plan
        .m_vecConformanceWarnings
        .iter()
        .any(|strWarning| strWarning.contains("Annex A.1")));
}

#[test]
fn a_disabled_override_is_ignored() {
    let mut simulation = LoadWith(vec![ResponseOverride {
        m_bIsEnabled: false,
        ..ResponseOverride::NewExact(&[0x22, 0xF1, 0x90], Substitute(&[0x7F, 0x22, 0x33]))
    }]);

    assert_eq!(
        &Send(&mut simulation, &[0x22, 0xF1, 0x90]).m_vecResponse[3..],
        b"SIMULATORVIN00001"
    );
}
