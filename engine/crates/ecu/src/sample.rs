//! Sample ECU configurations used by the demo, the dev API, and tests, so they all share one
//! definition instead of duplicating it.

#![allow(non_snake_case, non_upper_case_globals)]

use core_domain::model::{DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityLevel, SessionType};
use core_domain::Confidence;

/// Build a representative engine ECU: the Phase 1 service set, one DID (VIN), one DTC, and
/// one security level with a fixed seed/key pair.
pub fn BuildEngineEcu() -> Ecu {
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
