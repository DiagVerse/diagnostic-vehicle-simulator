//! What one correlated request/response pair tells us about an ECU.
//!
//! Everything here takes an ECU and two byte slices, and knows nothing about how they reached
//! us. That is deliberate: a data identifier learned over DoIP must be recorded exactly as one
//! learned over ISO-TP on CAN, and the only way to guarantee that is for both pipelines to run
//! the same code rather than two implementations that agree today.
//!
//! Extracted from `pipeline.rs` unchanged when the DoIP pipeline was added.

use core_domain::model::{DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityLevel, SessionType};
use core_domain::Confidence;

/// Offset between a request SID and its positive-response SID (ISO 14229).
pub(crate) const c_byPositiveResponseOffset: u8 = 0x40;
/// First byte of a negative response.
pub(crate) const c_byNegativeResponseSid: u8 = 0x7F;
/// Lowest and highest UDS request service identifiers (ISO 14229-1 clause 7.3). Requests fall
/// in two ranges; everything else on the bus is not a diagnostic request.
pub(crate) const c_bySidRequestLowFirst: u8 = 0x10;
pub(crate) const c_bySidRequestLowLast: u8 = 0x3E;
pub(crate) const c_bySidRequestHighFirst: u8 = 0x83;
pub(crate) const c_bySidRequestHighLast: u8 = 0x88;

/// Apply one correlated request/response pair to an ECU record.
pub(crate) fn ApplyPair(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
    let byServiceId = vecRequest[0];
    MarkServiceSupported(ecu, byServiceId);

    // A negative response still confirms the service exists; nothing else to extract.
    if vecResponse[0] == c_byNegativeResponseSid {
        return;
    }

    match byServiceId {
        0x10 => ApplySessionControl(ecu, vecRequest),
        0x22 => ApplyReadDataByIdentifier(ecu, vecRequest, vecResponse),
        0x19 => ApplyReadDtc(ecu, vecResponse),
        0x27 => ApplySecurityAccess(ecu, vecRequest, vecResponse),
        _ => {}
    }
}
pub(crate) fn MarkServiceSupported(ecu: &mut Ecu, byServiceId: u8) {
    if !ecu.m_vecSupportedServices.contains(&byServiceId) {
        ecu.m_vecSupportedServices.push(byServiceId);
    }
}
pub(crate) fn ApplySessionControl(ecu: &mut Ecu, vecRequest: &[u8]) {
    if vecRequest.len() < 2 {
        return;
    }
    let bySession = vecRequest[1] & 0x7F; // clear suppress-positive-response bit
    if let Some(session) = SessionType::FromSubFunction(bySession) {
        if !ecu.m_vecSupportedSessions.contains(&session) {
            ecu.m_vecSupportedSessions.push(session);
        }
    }
}
pub(crate) fn ApplyReadDataByIdentifier(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
    // Request: 22 DIDhi DIDlo ; Response: 62 DIDhi DIDlo <data...>
    if vecRequest.len() < 3 || vecResponse.len() < 3 {
        return;
    }
    let u16Did = ((vecResponse[1] as u16) << 8) | (vecResponse[2] as u16);
    let vecValue = vecResponse[3..].to_vec();
    ecu.m_mapDids.insert(
        u16Did,
        DataIdentifier {
            m_u16Id: u16Did,
            m_vecValue: vecValue,
            m_confidence: Confidence::Observed,
        },
    );
}
pub(crate) fn ApplyReadDtc(ecu: &mut Ecu, vecResponse: &[u8]) {
    // Response: 59 <sub> <statusAvailabilityMask> then repeated <b0 b1 b2 status>.
    if vecResponse.len() < 3 {
        return;
    }
    let vecRecords = &vecResponse[3..];
    let mut uIndex = 0;
    while uIndex + 4 <= vecRecords.len() {
        let u32Code = ((vecRecords[uIndex] as u32) << 16)
            | ((vecRecords[uIndex + 1] as u32) << 8)
            | (vecRecords[uIndex + 2] as u32);
        let byStatus = vecRecords[uIndex + 3];

        let bAlreadyKnown = ecu.m_vecDtcs.iter().any(|d| d.m_u32Code == u32Code);
        if !bAlreadyKnown {
            ecu.m_vecDtcs.push(DiagnosticTroubleCode {
                m_u32Code: u32Code,
                m_byStatus: byStatus,
                m_confidence: Confidence::Observed,
            });
        }
        uIndex += 4;
    }
}
pub(crate) fn ApplySecurityAccess(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
    // Only requestSeed (odd sub-function) reveals a seed. The key/algorithm is never
    // observable from a trace, so it stays Unknown (empty expected key).
    if vecRequest.len() < 2 {
        return;
    }
    let bySubFunction = vecRequest[1];
    let bIsRequestSeed = (bySubFunction & 0x01) == 0x01;
    if !bIsRequestSeed {
        return;
    }

    let vecSeed = if vecResponse.len() > 2 {
        vecResponse[2..].to_vec()
    } else {
        Vec::new()
    };

    let bAlreadyKnown = ecu
        .m_vecSecurityLevels
        .iter()
        .any(|l| l.m_byRequestSeedSubFunction == bySubFunction);
    if !bAlreadyKnown {
        ecu.m_vecSecurityLevels.push(SecurityLevel {
            m_byRequestSeedSubFunction: bySubFunction,
            m_vecSeed: vecSeed,
            m_vecExpectedKey: Vec::new(),
        });
    }
}
/// True for a UDS request service identifier (ISO 14229-1 clause 7.3, Table 2).
///
/// This is a whitelist rather than "anything below 0x40" because a CAN log is mostly ordinary
/// periodic traffic: a powertrain frame whose first byte happens to look like a valid ISO-TP
/// single-frame header would otherwise be taken for a diagnostic request and pollute
/// correlation. The high range 0x83..=0x88 (AccessTimingParameter, ControlDTCSetting,
/// LinkControl, …) is included; it appears in real flashing sequences.
pub(crate) fn IsRequestSid(byFirst: u8) -> bool {
    let bIsLowRange = (c_bySidRequestLowFirst..=c_bySidRequestLowLast).contains(&byFirst);
    let bIsHighRange = (c_bySidRequestHighFirst..=c_bySidRequestHighLast).contains(&byFirst);
    bIsLowRange || bIsHighRange
}
