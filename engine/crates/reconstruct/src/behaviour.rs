//! What one correlated request/response pair tells us about an ECU.
//!
//! Everything here takes an ECU and two byte slices, and knows nothing about how they reached
//! us. That is deliberate: a data identifier learned over DoIP must be recorded exactly as one
//! learned over ISO-TP on CAN, and the only way to guarantee that is for both pipelines to run
//! the same code rather than two implementations that agree today.
//!
//! Extracted from `pipeline.rs` unchanged when the DoIP pipeline was added.

use core_domain::model::{
    DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityKeyPolicy, SecurityLevel, SessionType,
};
use core_domain::Confidence;

/// Offset between a request SID and its positive-response SID (ISO 14229).
pub(crate) const c_byPositiveResponseOffset: u8 = 0x40;
/// First byte of a negative response.
pub(crate) const c_byNegativeResponseSid: u8 = 0x7F;
/// Every request service identifier ISO 14229-1 defines (Table 2), and nothing else.
///
/// A list rather than the two contiguous ranges this used to test. The ranges include values
/// the standard leaves undefined — 0x13, 0x15, 0x3A and a dozen more — and on a bus carrying
/// thousands of periodic frames a second, every undefined value in the range is another way for
/// an ordinary powertrain frame to be mistaken for a diagnostic request. A real CANoe capture
/// of a vehicle with no diagnostics on it at all reconstructed into four ECUs that way.
pub(crate) const c_arrRequestSids: [u8; 27] = [
    0x10, // DiagnosticSessionControl
    0x11, // ECUReset
    0x14, // ClearDiagnosticInformation
    0x19, // ReadDTCInformation
    0x22, // ReadDataByIdentifier
    0x23, // ReadMemoryByAddress
    0x24, // ReadScalingDataByIdentifier
    0x27, // SecurityAccess
    0x28, // CommunicationControl
    0x29, // Authentication
    0x2A, // ReadDataByPeriodicIdentifier
    0x2C, // DynamicallyDefineDataIdentifier
    0x2E, // WriteDataByIdentifier
    0x2F, // InputOutputControlByIdentifier
    0x31, // RoutineControl
    0x34, // RequestDownload
    0x35, // RequestUpload
    0x36, // TransferData
    0x37, // RequestTransferExit
    0x38, // RequestFileTransfer
    0x3D, // WriteMemoryByAddress
    0x3E, // TesterPresent
    0x83, // AccessTimingParameter
    0x84, // SecuredDataTransmission
    0x85, // ControlDTCSetting
    0x86, // ResponseOnEvent
    0x87, // LinkControl
];

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
            // A capture never yields a usable key. Even when the trace contains the sendKey a
            // tester sent, that key answered *that* seed; a fresh session issues a fresh seed
            // and the recorded key is wrong by construction. Comparing against an empty key
            // would refuse every tester with NRC 0x35 and look like a simulator bug, so a
            // reconstructed level accepts instead — and says so through its policy rather than
            // by pretending to hold a key it does not have.
            m_keyPolicy: SecurityKeyPolicy::AcceptAnyKey,
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
/// NRC 0x78 requestCorrectlyReceived-ResponsePending (ISO 14229-1 Annex A.1).
pub(crate) const c_byNrcResponsePending: u8 = 0x78;

/// True for an interim "still working" answer rather than a final one.
///
/// It matters to correlation because `7F <sid> 78` looks exactly like a refusal and is not one:
/// the real answer is still coming. Treating it as final retires the pending request, and the
/// response that follows then matches nothing and is dropped — so every read that went through
/// a ResponsePending, which on a real ECU is most of them, contributed a "service supported"
/// mark and no value at all.
pub(crate) fn IsResponsePending(vecResponse: &[u8]) -> bool {
    vecResponse.len() >= 3
        && vecResponse[0] == c_byNegativeResponseSid
        && vecResponse[2] == c_byNrcResponsePending
}

pub(crate) fn IsRequestSid(byFirst: u8) -> bool {
    c_arrRequestSids.contains(&byFirst)
}
