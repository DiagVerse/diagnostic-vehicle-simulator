//! Reconstruction pipeline: frames -> ISO-TP PDUs -> correlated UDS pairs -> Vehicle model.
//!
//! See ADR 0003 for the design. Every fact produced here is `Confidence::Observed` because it
//! was seen in a trace rather than taken from a specification.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use can::CanFrame;
use core_domain::model::{
    DataIdentifier, DiagnosticTroubleCode, Ecu, SecurityLevel, SessionType, Vehicle,
};
use core_domain::Confidence;
use isotp::ReassembleStream;

/// Offset between a request SID and its positive-response SID (ISO 14229).
const c_byPositiveResponseOffset: u8 = 0x40;
/// First byte of a negative response.
const c_byNegativeResponseSid: u8 = 0x7F;

/// One reassembled PDU tagged with the CAN ID it arrived on and its time.
struct PduRecord {
    m_u32CanId: u32,
    m_f64TimestampSec: f64,
    m_vecBytes: Vec<u8>,
}

/// A request awaiting its response during correlation.
struct PendingRequest {
    m_byServiceId: u8,
    m_vecBytes: Vec<u8>,
}

/// Reconstruct a vehicle model from time-ordered CAN frames.
pub fn ReconstructFromFrames(vecFrames: &[CanFrame]) -> Vehicle {
    let vecPdus = ReassembleAllStreams(vecFrames);

    // ECUs keyed by their response CAN ID (the ECU's transmit identifier).
    let mut mapEcus: BTreeMap<u32, Ecu> = BTreeMap::new();
    let mut optPending: Option<PendingRequest> = None;

    for pdu in &vecPdus {
        if pdu.m_vecBytes.is_empty() {
            continue;
        }
        let byFirst = pdu.m_vecBytes[0];

        // Try to interpret this PDU as the response to the pending request.
        if let Some(pending) = optPending.take() {
            if IsResponseTo(&pending, &pdu.m_vecBytes) {
                let ecu = EcuFor(&mut mapEcus, pdu.m_u32CanId);
                ApplyPair(ecu, &pending.m_vecBytes, &pdu.m_vecBytes);
                continue;
            }
            // Not a response — fall through and treat `pdu` as a possible new request.
        }

        if IsRequestSid(byFirst) {
            optPending = Some(PendingRequest {
                m_byServiceId: byFirst,
                m_vecBytes: pdu.m_vecBytes.clone(),
            });
        }
        // Unmatched responses are ignored.
    }

    Vehicle {
        m_strName: "Reconstructed Vehicle".to_string(),
        m_vecEcus: mapEcus.into_values().collect(),
    }
}

/// Reassemble every CAN-ID stream and return all PDUs in global time order.
fn ReassembleAllStreams(vecFrames: &[CanFrame]) -> Vec<PduRecord> {
    let mut mapById: BTreeMap<u32, Vec<CanFrame>> = BTreeMap::new();
    for frame in vecFrames {
        mapById
            .entry(frame.m_u32CanId)
            .or_default()
            .push(frame.clone());
    }

    let mut vecPdus = Vec::new();
    for (u32CanId, vecStream) in &mapById {
        for msg in ReassembleStream(vecStream) {
            vecPdus.push(PduRecord {
                m_u32CanId: *u32CanId,
                m_f64TimestampSec: msg.m_f64TimestampSec,
                m_vecBytes: msg.m_vecData,
            });
        }
    }

    // Global time order so request/response correlation follows the real exchange.
    vecPdus.sort_by(|a, b| {
        a.m_f64TimestampSec
            .partial_cmp(&b.m_f64TimestampSec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    vecPdus
}

/// A request SID (the ones this phase understands are all below 0x40).
fn IsRequestSid(byFirst: u8) -> bool {
    byFirst < 0x40
}

/// True if `vecResponse` is the positive or negative response to `pending`.
fn IsResponseTo(pending: &PendingRequest, vecResponse: &[u8]) -> bool {
    let byFirst = vecResponse[0];
    let bIsPositive = byFirst == pending.m_byServiceId + c_byPositiveResponseOffset;
    let bIsNegative =
        byFirst == c_byNegativeResponseSid && vecResponse.get(1) == Some(&pending.m_byServiceId);
    bIsPositive || bIsNegative
}

/// Get (or create) the ECU record for a response CAN ID.
fn EcuFor(mapEcus: &mut BTreeMap<u32, Ecu>, u32ResponseId: u32) -> &mut Ecu {
    mapEcus
        .entry(u32ResponseId)
        .or_insert_with(|| Ecu::New(&format!("ECU_{u32ResponseId:03X}"), u32ResponseId as u16))
}

/// Apply one correlated request/response pair to an ECU record.
fn ApplyPair(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
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

fn MarkServiceSupported(ecu: &mut Ecu, byServiceId: u8) {
    if !ecu.m_vecSupportedServices.contains(&byServiceId) {
        ecu.m_vecSupportedServices.push(byServiceId);
    }
}

fn ApplySessionControl(ecu: &mut Ecu, vecRequest: &[u8]) {
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

fn ApplyReadDataByIdentifier(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
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

fn ApplyReadDtc(ecu: &mut Ecu, vecResponse: &[u8]) {
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

fn ApplySecurityAccess(ecu: &mut Ecu, vecRequest: &[u8], vecResponse: &[u8]) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: u32, t: f64, data: Vec<u8>) -> CanFrame {
        CanFrame::NewClassic(t, id, data)
    }

    #[test]
    fn reconstructs_session_did_and_dtc() {
        // A small single-frame exchange on 0x7E0 (request) / 0x7E8 (response).
        let frames = vec![
            f(0x7E0, 0.001, vec![0x02, 0x10, 0x03]),
            f(0x7E8, 0.002, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
            f(0x7E0, 0.003, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x7E8, 0.004, vec![0x06, 0x62, 0xF1, 0x90, 0x41, 0x42, 0x43]),
            f(0x7E0, 0.005, vec![0x03, 0x19, 0x02, 0xFF]),
            f(
                0x7E8,
                0.006,
                vec![0x07, 0x59, 0x02, 0xFF, 0x12, 0x34, 0x56, 0x2F],
            ),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 1);

        let ecu = &vehicle.m_vecEcus[0];
        assert_eq!(ecu.m_u16LogicalAddress, 0x7E8);
        assert!(ecu.m_vecSupportedServices.contains(&0x10));
        assert!(ecu.m_vecSupportedServices.contains(&0x22));
        assert!(ecu.m_vecSupportedServices.contains(&0x19));
        assert!(ecu.m_vecSupportedSessions.contains(&SessionType::Extended));
        assert_eq!(
            ecu.FindDid(0xF190).unwrap().m_vecValue,
            vec![0x41, 0x42, 0x43]
        );
        assert_eq!(ecu.m_vecDtcs.len(), 1);
        assert_eq!(ecu.m_vecDtcs[0].m_u32Code, 0x123456);
    }

    #[test]
    fn negative_response_still_marks_service_supported() {
        let frames = vec![
            f(0x7E0, 0.001, vec![0x02, 0x27, 0x01]),
            f(0x7E8, 0.002, vec![0x03, 0x7F, 0x27, 0x33]), // securityAccessDenied
        ];
        let vehicle = ReconstructFromFrames(&frames);
        let ecu = &vehicle.m_vecEcus[0];
        assert!(ecu.m_vecSupportedServices.contains(&0x27));
        // No seed was revealed, so no security level is recorded.
        assert!(ecu.m_vecSecurityLevels.is_empty());
    }
}
