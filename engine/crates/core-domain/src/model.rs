//! Unified Vehicle Model — Phase 1 subset.
//!
//! This module holds the pure data model an ECU simulation operates on: the ECUs, the
//! diagnostic services/DIDs/DTCs they expose, the sessions they support, and their security
//! configuration. It is **configuration + observed facts only** — the live, mutable runtime
//! state of a running ECU (current session, whether security is unlocked) lives in the `ecu`
//! runtime crate, not here.
//!
//! Naming follows the project convention (see CLAUDE.md), which requires allowing Rust's
//! `non_snake_case` lint for this module. Serialized structs use `serde(rename_all)` so the
//! persisted JSON stays readable.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Confidence;

/// UDS diagnostic session type — the sub-function of service 0x10 (DiagnosticSessionControl,
/// ISO 14229). A running ECU is always in exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionType {
    /// 0x01 — the session an ECU powers up in.
    #[default]
    Default,
    /// 0x02 — used for reprogramming/flashing.
    Programming,
    /// 0x03 — unlocks extended diagnostics.
    Extended,
    /// 0x04 — safety-system diagnostics.
    SafetySystem,
}

impl SessionType {
    /// Map to the UDS sub-function byte used on the wire.
    pub fn ToSubFunction(self) -> u8 {
        match self {
            SessionType::Default => 0x01,
            SessionType::Programming => 0x02,
            SessionType::Extended => 0x03,
            SessionType::SafetySystem => 0x04,
        }
    }

    /// Map a UDS sub-function byte back to a session type. Returns `None` for unknown values
    /// so the caller can answer with the appropriate negative response.
    pub fn FromSubFunction(bySubFunction: u8) -> Option<SessionType> {
        match bySubFunction {
            0x01 => Some(SessionType::Default),
            0x02 => Some(SessionType::Programming),
            0x03 => Some(SessionType::Extended),
            0x04 => Some(SessionType::SafetySystem),
            _ => None,
        }
    }
}

/// A Data Identifier (DID) and the value the ECU returns for it (service 0x22).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataIdentifier {
    /// 16-bit DID, e.g. 0xF190 (VIN).
    pub m_u16Id: u16,
    /// Raw bytes the ECU reports for this DID.
    pub m_vecValue: Vec<u8>,
    /// How the value was established (specified, observed in a trace, inferred, …).
    pub m_confidence: Confidence,
}

/// A stored Diagnostic Trouble Code (reported by service 0x19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticTroubleCode {
    /// 3-byte DTC packed into a u32 (ISO 14229 DTC format), e.g. 0x123456.
    pub m_u32Code: u32,
    /// DTC status byte (ISO 14229-1 status-of-DTC bitfield).
    pub m_byStatus: u8,
    /// How this DTC was established.
    pub m_confidence: Confidence,
}

/// Security-access configuration for a single level (service 0x27).
///
/// Phase 1 uses a deliberately simple fixed seed/key pair per level so behaviour is
/// deterministic and testable. Real seed/key algorithms arrive with ODX/security phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityLevel {
    /// The requestSeed sub-function that identifies this level (odd value, e.g. 0x01).
    pub m_byRequestSeedSubFunction: u8,
    /// The seed the ECU returns when this level's seed is requested.
    pub m_vecSeed: Vec<u8>,
    /// The key the ECU expects back to unlock this level.
    pub m_vecExpectedKey: Vec<u8>,
}

impl SecurityLevel {
    /// The sendKey sub-function paired with this level (requestSeed + 1, per ISO 14229).
    pub fn SendKeySubFunction(&self) -> u8 {
        self.m_byRequestSeedSubFunction.wrapping_add(1)
    }
}

/// UDS timing parameters (milliseconds). Used later to drive ResponsePending/timeout
/// behaviour; carried in the model now so it need not be reshaped later.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EcuTiming {
    /// P2server_max — max time to answer a request before a pending response is required.
    pub m_u32P2ServerMaxMs: u32,
    /// P2*server_max — max time between ResponsePending (NRC 0x78) messages.
    pub m_u32P2StarServerMaxMs: u32,
}

impl Default for EcuTiming {
    fn default() -> Self {
        // ISO 14229 default-ish values; adjustable per ECU.
        EcuTiming {
            m_u32P2ServerMaxMs: 50,
            m_u32P2StarServerMaxMs: 5000,
        }
    }
}

/// A single virtual ECU's static diagnostic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ecu {
    /// Human-readable name, e.g. "Engine_ECU".
    pub m_strName: String,
    /// UDS logical/diagnostic address (used for DoIP and addressing later).
    pub m_u16LogicalAddress: u16,
    /// Service IDs (request SIDs) this ECU supports, e.g. 0x10, 0x22, 0x27.
    pub m_vecSupportedServices: Vec<u8>,
    /// Sessions this ECU can enter.
    pub m_vecSupportedSessions: Vec<SessionType>,
    /// DIDs keyed by identifier, so lookups for service 0x22 are ordered and fast.
    pub m_mapDids: BTreeMap<u16, DataIdentifier>,
    /// Stored DTCs reported by service 0x19.
    pub m_vecDtcs: Vec<DiagnosticTroubleCode>,
    /// Security levels this ECU supports (service 0x27).
    pub m_vecSecurityLevels: Vec<SecurityLevel>,
    /// Timing parameters.
    pub m_timing: EcuTiming,
}

impl Ecu {
    /// Create an ECU with the given name/address and no capabilities yet. Callers populate
    /// the collections explicitly, which keeps ECU construction readable at call sites.
    pub fn New(strName: &str, u16LogicalAddress: u16) -> Self {
        Ecu {
            m_strName: strName.to_string(),
            m_u16LogicalAddress: u16LogicalAddress,
            m_vecSupportedServices: Vec::new(),
            m_vecSupportedSessions: vec![SessionType::Default],
            m_mapDids: BTreeMap::new(),
            m_vecDtcs: Vec::new(),
            m_vecSecurityLevels: Vec::new(),
            m_timing: EcuTiming::default(),
        }
    }

    /// True if the ECU advertises support for the given request service id.
    pub fn IsServiceSupported(&self, byServiceId: u8) -> bool {
        self.m_vecSupportedServices.contains(&byServiceId)
    }

    /// True if the ECU can enter the given session.
    pub fn IsSessionSupported(&self, session: SessionType) -> bool {
        self.m_vecSupportedSessions.contains(&session)
    }

    /// Look up a DID's configured value.
    pub fn FindDid(&self, u16Id: u16) -> Option<&DataIdentifier> {
        self.m_mapDids.get(&u16Id)
    }

    /// Find the security level identified by a requestSeed sub-function.
    pub fn FindSecurityLevelByRequestSeed(&self, bySubFunction: u8) -> Option<&SecurityLevel> {
        self.m_vecSecurityLevels
            .iter()
            .find(|level| level.m_byRequestSeedSubFunction == bySubFunction)
    }

    /// Find the security level whose sendKey sub-function matches the given value.
    pub fn FindSecurityLevelBySendKey(&self, bySubFunction: u8) -> Option<&SecurityLevel> {
        self.m_vecSecurityLevels
            .iter()
            .find(|level| level.SendKeySubFunction() == bySubFunction)
    }
}

/// The whole reconstructed/simulated vehicle: a collection of ECUs. Networks, gateways and
/// routing join this in later phases.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Vehicle {
    /// Vehicle name/identifier.
    pub m_strName: String,
    /// All ECUs in the vehicle.
    pub m_vecEcus: Vec<Ecu>,
}

impl Vehicle {
    /// Serialize to pretty JSON for persistence/inspection.
    pub fn ToJson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Load a vehicle from JSON.
    pub fn FromJson(strJson: &str) -> Result<Vehicle, serde_json::Error> {
        serde_json::from_str(strJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_type_maps_to_and_from_sub_function() {
        assert_eq!(SessionType::Extended.ToSubFunction(), 0x03);
        assert_eq!(
            SessionType::FromSubFunction(0x02),
            Some(SessionType::Programming)
        );
        assert_eq!(SessionType::FromSubFunction(0xFF), None);
    }

    #[test]
    fn ecu_lookups_work() {
        let mut ecu = Ecu::New("Engine_ECU", 0x1001);
        ecu.m_vecSupportedServices.push(0x22);
        ecu.m_mapDids.insert(
            0xF190,
            DataIdentifier {
                m_u16Id: 0xF190,
                m_vecValue: b"VIN0123456789".to_vec(),
                m_confidence: Confidence::Observed,
            },
        );

        assert!(ecu.IsServiceSupported(0x22));
        assert!(!ecu.IsServiceSupported(0x27));
        assert!(ecu.IsSessionSupported(SessionType::Default));
        assert_eq!(ecu.FindDid(0xF190).unwrap().m_vecValue, b"VIN0123456789");
        assert!(ecu.FindDid(0x1234).is_none());
    }

    #[test]
    fn security_level_pairs_seed_and_key_sub_functions() {
        let level = SecurityLevel {
            m_byRequestSeedSubFunction: 0x01,
            m_vecSeed: vec![0x11, 0x22],
            m_vecExpectedKey: vec![0x33, 0x44],
        };
        assert_eq!(level.SendKeySubFunction(), 0x02);
    }

    #[test]
    fn vehicle_round_trips_through_json() {
        let mut vehicle = Vehicle {
            m_strName: "TestVehicle".to_string(),
            m_vecEcus: Vec::new(),
        };
        vehicle.m_vecEcus.push(Ecu::New("Engine_ECU", 0x1001));

        let strJson = vehicle.ToJson().unwrap();
        let loaded = Vehicle::FromJson(&strJson).unwrap();

        assert_eq!(loaded.m_strName, "TestVehicle");
        assert_eq!(loaded.m_vecEcus.len(), 1);
        assert_eq!(loaded.m_vecEcus[0].m_u16LogicalAddress, 0x1001);
    }
}
