//! End-to-end Phase 1 test: a scripted diagnostic session against a virtual ECU driven by
//! the real UDS protocol logic. This exercises the whole engine loop — snapshot → protocol
//! → state changes → response — and asserts both the response bytes and the resulting ECU
//! state transitions, matching the Phase 1 acceptance criteria in the plan.

#![allow(non_snake_case)]

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::{DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityLevel, SessionType};
use core_domain::Confidence;
use ecu::VirtualEcu;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};

/// Bridges the UDS plugin's pure handler to the engine's `ProtocolHandler` trait. In
/// production this is a dynamically-loaded plugin; here we link the same logic directly so
/// the test is deterministic and needs no built dynamic library.
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

/// Build an Engine ECU that supports the Phase 1 service set, one DID, one DTC, and one
/// security level with a fixed seed/key.
fn MakeEngineEcu() -> Ecu {
    let mut ecu = Ecu::New("Engine_ECU", 0x1001);

    ecu.m_vecSupportedServices = vec![0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x3E];
    ecu.m_vecSupportedSessions = vec![
        SessionType::Default,
        SessionType::Programming,
        SessionType::Extended,
    ];

    ecu.m_mapDids.insert(
        0xF190,
        DataIdentifier {
            m_u16Id: 0xF190,
            m_vecValue: b"VIN0123456789XYZ".to_vec(),
            m_confidence: Confidence::Observed,
        },
    );

    ecu.m_vecDtcs.push(DiagnosticTroubleCode {
        m_u32Code: 0x123456,
        m_byStatus: 0x2F,
        m_confidence: Confidence::Observed,
    });

    ecu.m_vecSecurityLevels.push(SecurityLevel {
        m_byRequestSeedSubFunction: 0x01,
        m_vecSeed: vec![0x11, 0x22, 0x33, 0x44],
        m_vecExpectedKey: vec![0xAA, 0xBB, 0xCC, 0xDD],
    });

    ecu
}

#[test]
fn full_diagnostic_session_flow() {
    let handler = UdsHandler;
    let mut ecu = VirtualEcu::New(MakeEngineEcu());

    // 1. Read the VIN in the default session — allowed in any session.
    let response = ecu.ProcessRequest(&handler, &[0x22, 0xF1, 0x90]);
    assert_eq!(&response[0..3], &[0x62, 0xF1, 0x90]);
    assert_eq!(&response[3..], b"VIN0123456789XYZ");

    // 2. Security access is denied while still in the default session.
    let response = ecu.ProcessRequest(&handler, &[0x27, 0x01]);
    assert_eq!(response, vec![0x7F, 0x27, 0x7F]); // serviceNotSupportedInActiveSession
    assert!(!ecu.IsSecurityUnlocked());

    // 3. Enter the extended session.
    let response = ecu.ProcessRequest(&handler, &[0x10, 0x03]);
    assert_eq!(response[0], 0x50);
    assert_eq!(response[1], 0x03);
    assert_eq!(ecu.CurrentSession(), 0x03);

    // 4. Request the security seed.
    let response = ecu.ProcessRequest(&handler, &[0x27, 0x01]);
    assert_eq!(&response[0..2], &[0x67, 0x01]);
    assert_eq!(&response[2..], &[0x11, 0x22, 0x33, 0x44]);

    // 5. Send the correct key and unlock security.
    let response = ecu.ProcessRequest(&handler, &[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(response, vec![0x67, 0x02]);
    assert!(ecu.IsSecurityUnlocked());
    assert_eq!(ecu.SecurityUnlockedLevel(), 0x01);

    // 6. Reset the ECU: back to default session, security relocked.
    let response = ecu.ProcessRequest(&handler, &[0x11, 0x01]);
    assert_eq!(response, vec![0x51, 0x01]);
    assert_eq!(ecu.CurrentSession(), 0x01);
    assert!(!ecu.IsSecurityUnlocked());

    // 7. TesterPresent keep-alive.
    let response = ecu.ProcessRequest(&handler, &[0x3E, 0x00]);
    assert_eq!(response, vec![0x7E, 0x00]);
}

#[test]
fn wrong_key_does_not_unlock() {
    let handler = UdsHandler;
    let mut ecu = VirtualEcu::New(MakeEngineEcu());

    ecu.ProcessRequest(&handler, &[0x10, 0x03]); // extended
    ecu.ProcessRequest(&handler, &[0x27, 0x01]); // request seed

    let response = ecu.ProcessRequest(&handler, &[0x27, 0x02, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(response, vec![0x7F, 0x27, 0x35]); // invalidKey
    assert!(!ecu.IsSecurityUnlocked());
}
