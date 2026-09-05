//! Reconstruction pipeline: frames -> ISO-TP PDUs -> correlated UDS pairs -> Vehicle model.
//!
//! See ADR 0003 for the design. Every fact produced here is `Confidence::Observed` because it
//! was seen in a trace rather than taken from a specification.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use can::CanFrame;
use core_domain::model::{
    c_u32Response11BitOffset, CanAddress, CanAddressingMode, DataIdentifier, DiagnosticTroubleCode,
    Ecu, SecurityLevel, SessionType, Vehicle,
};
use core_domain::Confidence;
use isotp::ReassembleStream;

/// Offset between a request SID and its positive-response SID (ISO 14229).
const c_byPositiveResponseOffset: u8 = 0x40;
/// First byte of a negative response.
const c_byNegativeResponseSid: u8 = 0x7F;
/// The OBD/UDS functional (broadcast) request identifier: every ECU on the bus listens to it,
/// so it is never one ECU's own request identifier (ISO 15765-4).
const c_u32FunctionalRequestCanId: u32 = 0x7DF;
/// The fixed high half of a 29-bit normal-fixed physical identifier (ISO 15765-2):
/// 0x18DA<target><source>.
const c_u32NormalFixed29BitPrefix: u32 = 0x18DA_0000;
/// Upper bound on outstanding unanswered requests kept during correlation. Logs routinely
/// contain requests nothing ever answers; without a cap the list would grow with the log.
const c_uMaxPendingRequests: usize = 64;

/// One reassembled PDU tagged with the CAN ID it arrived on and its time.
struct PduRecord {
    m_u32CanId: u32,
    m_f64TimestampSec: f64,
    m_vecBytes: Vec<u8>,
}

/// A request awaiting its response during correlation.
struct PendingRequest {
    /// CAN identifier the request was sent on (physical, e.g. 0x7E0, or functional 0x7DF).
    m_u32RequestCanId: u32,
    m_byServiceId: u8,
    m_vecBytes: Vec<u8>,
}

/// Reconstruct a vehicle model from time-ordered CAN frames.
pub fn ReconstructFromFrames(vecFrames: &[CanFrame]) -> Vehicle {
    let vecPdus = ReassembleAllStreams(vecFrames);

    // ECUs keyed by their response CAN ID (the ECU's transmit identifier — unique per ECU).
    let mut mapEcus: BTreeMap<u32, Ecu> = BTreeMap::new();
    // Requests seen but not yet answered, oldest first. A log can interleave exchanges with
    // several ECUs, so more than one request may be outstanding at a time.
    let mut vecPending: Vec<PendingRequest> = Vec::new();

    for pdu in &vecPdus {
        if pdu.m_vecBytes.is_empty() {
            continue;
        }

        // A PDU is first tested as an answer to something outstanding; only if it answers
        // nothing is it considered as a new request.
        if let Some(uIndex) = FindPendingRequestFor(&vecPending, pdu) {
            let pending = vecPending.remove(uIndex);
            let ecu = EcuFor(&mut mapEcus, pdu.m_u32CanId);
            RecordCanAddress(ecu, pending.m_u32RequestCanId, pdu.m_u32CanId);
            ApplyPair(ecu, &pending.m_vecBytes, &pdu.m_vecBytes);
            continue;
        }

        if IsRequestSid(pdu.m_vecBytes[0]) {
            RememberRequest(&mut vecPending, pdu);
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

/// Record a newly seen request as outstanding.
///
/// A second request on an identifier that already has one outstanding replaces it: the older
/// request evidently went unanswered, and keeping it would let it steal a later ECU's
/// response. The list is also capped so a log full of unanswered requests cannot grow it
/// without bound.
fn RememberRequest(vecPending: &mut Vec<PendingRequest>, pdu: &PduRecord) {
    vecPending.retain(|pending| pending.m_u32RequestCanId != pdu.m_u32CanId);

    if vecPending.len() >= c_uMaxPendingRequests {
        vecPending.remove(0);
    }

    vecPending.push(PendingRequest {
        m_u32RequestCanId: pdu.m_u32CanId,
        m_byServiceId: pdu.m_vecBytes[0],
        m_vecBytes: pdu.m_vecBytes.clone(),
    });
}

/// Find which outstanding request `pdu` answers, if any.
///
/// Two rules, in order:
///   1. **Conventional pair** — a request identifier whose conventional response identifier is
///      the one this PDU arrived on (0x7E0 -> 0x7E8, or the 29-bit normal-fixed swap). This is
///      the strong signal and is checked first so interleaved exchanges with several ECUs
///      cannot cross-match.
///   2. **Most recent outstanding request** — the fallback that covers OEM-specific identifier
///      pairs, which follow no derivable rule, and functional requests on 0x7DF.
fn FindPendingRequestFor(vecPending: &[PendingRequest], pdu: &PduRecord) -> Option<usize> {
    let vecCandidates: Vec<usize> = vecPending
        .iter()
        .enumerate()
        .filter(|(_, pending)| IsResponseTo(pending, &pdu.m_vecBytes))
        .map(|(uIndex, _)| uIndex)
        .collect();

    for uIndex in &vecCandidates {
        let pending = &vecPending[*uIndex];
        if DeriveResponseCanId(pending.m_u32RequestCanId) == Some(pdu.m_u32CanId) {
            return Some(*uIndex);
        }
    }

    // Most recently seen request first.
    vecCandidates.last().copied()
}

/// The response identifier conventionally paired with a request identifier, or `None` when no
/// convention applies (e.g. the functional identifier 0x7DF, which every ECU answers on its
/// own physical identifier).
fn DeriveResponseCanId(u32RequestCanId: u32) -> Option<u32> {
    if u32RequestCanId == c_u32FunctionalRequestCanId {
        return None;
    }
    if IsNormal11BitRequestId(u32RequestCanId) {
        return Some(u32RequestCanId + c_u32Response11BitOffset);
    }
    if let Some((byTarget, bySource)) = SplitNormalFixed29BitId(u32RequestCanId) {
        // 29-bit normal fixed (ISO 15765-2): the answer swaps target and source.
        return Some(BuildNormalFixed29BitId(bySource, byTarget));
    }
    None
}

/// The request identifier conventionally paired with a response identifier, or `None` when the
/// pairing cannot be derived.
fn DeriveRequestCanId(u32ResponseCanId: u32) -> Option<u32> {
    if IsNormal11BitResponseId(u32ResponseCanId) {
        return Some(u32ResponseCanId - c_u32Response11BitOffset);
    }
    if let Some((byTarget, bySource)) = SplitNormalFixed29BitId(u32ResponseCanId) {
        return Some(BuildNormalFixed29BitId(bySource, byTarget));
    }
    None
}

/// True for the conventional 11-bit UDS request identifiers 0x7E0..=0x7E7 (ISO 15765-4).
fn IsNormal11BitRequestId(u32CanId: u32) -> bool {
    (0x7E0..=0x7E7).contains(&u32CanId)
}

/// True for the conventional 11-bit UDS response identifiers 0x7E8..=0x7EF (ISO 15765-4).
fn IsNormal11BitResponseId(u32CanId: u32) -> bool {
    (0x7E8..=0x7EF).contains(&u32CanId)
}

/// Split a 29-bit normal-fixed physical identifier (0x18DA<target><source>) into its target
/// and source addresses, or `None` if it is not one.
fn SplitNormalFixed29BitId(u32CanId: u32) -> Option<(u8, u8)> {
    if (u32CanId & 0xFFFF0000) != c_u32NormalFixed29BitPrefix {
        return None;
    }
    let byTarget = ((u32CanId >> 8) & 0xFF) as u8;
    let bySource = (u32CanId & 0xFF) as u8;
    Some((byTarget, bySource))
}

/// Build a 29-bit normal-fixed physical identifier from a target and source address.
fn BuildNormalFixed29BitId(byTarget: u8, bySource: u8) -> u32 {
    c_u32NormalFixed29BitPrefix | ((byTarget as u32) << 8) | (bySource as u32)
}

/// Record the CAN identifier pair an ECU is reached on.
///
/// When the request was physically addressed, both identifiers were seen on the bus and the
/// pair is `Observed`. When it was functionally addressed (0x7DF), the ECU's own request
/// identifier was never on the bus, so it is derived from the response identifier by
/// convention and recorded as `Inferred`. An already-`Observed` pair is never downgraded by a
/// later inference.
fn RecordCanAddress(ecu: &mut Ecu, u32RequestCanId: u32, u32ResponseCanId: u32) {
    let bIsFunctional = u32RequestCanId == c_u32FunctionalRequestCanId;

    let (u32PhysicalRequestId, confidence) = if bIsFunctional {
        match DeriveRequestCanId(u32ResponseCanId) {
            Some(u32Derived) => (u32Derived, Confidence::Inferred),
            // Nothing to derive from: keep whatever we already have rather than recording the
            // shared broadcast identifier as if it belonged to this ECU.
            None => return,
        }
    } else {
        (u32RequestCanId, Confidence::Observed)
    };

    let bWouldDowngrade = matches!(
        ecu.m_optCanAddress,
        Some(existing) if existing.m_confidence == Confidence::Observed
    ) && confidence == Confidence::Inferred;
    if bWouldDowngrade {
        return;
    }

    ecu.m_optCanAddress = Some(CanAddress {
        m_u32RequestCanId: u32PhysicalRequestId,
        m_u32ResponseCanId: u32ResponseCanId,
        m_addressingMode: AddressingModeOf(u32PhysicalRequestId, u32ResponseCanId),
        m_confidence: confidence,
    });
}

/// Classify an identifier pair's addressing mode. Anything outside the 29-bit normal-fixed
/// range is treated as normal 11-bit addressing, which is what the MVP simulates.
fn AddressingModeOf(u32RequestCanId: u32, u32ResponseCanId: u32) -> CanAddressingMode {
    let bBothNormalFixed = SplitNormalFixed29BitId(u32RequestCanId).is_some()
        && SplitNormalFixed29BitId(u32ResponseCanId).is_some();
    if bBothNormalFixed {
        CanAddressingMode::NormalFixed29Bit
    } else {
        CanAddressingMode::Normal11Bit
    }
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
    mapEcus.entry(u32ResponseId).or_insert_with(|| {
        Ecu::New(
            &format!("ECU_{u32ResponseId:03X}"),
            LogicalAddressOf(u32ResponseId),
        )
    })
}

/// Derive an ECU's logical (diagnostic) address from the identifier it answers on.
///
/// For 29-bit normal-fixed addressing the ECU's own address is the source byte of its response
/// identifier. For 11-bit identifiers there is no separate logical address in the frame, so the
/// response identifier itself stands in until a specification supplies the real one.
fn LogicalAddressOf(u32ResponseCanId: u32) -> u16 {
    match SplitNormalFixed29BitId(u32ResponseCanId) {
        Some((_byTarget, bySource)) => bySource as u16,
        None => (u32ResponseCanId & 0xFFFF) as u16,
    }
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
    fn records_the_observed_request_and_response_can_ids() {
        let frames = vec![
            f(0x7E0, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x7E8, 0.002, vec![0x06, 0x62, 0xF1, 0x90, 0x41, 0x42, 0x43]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        let address = vehicle.m_vecEcus[0]
            .m_optCanAddress
            .expect("CAN address recorded");

        assert_eq!(address.m_u32RequestCanId, 0x7E0);
        assert_eq!(address.m_u32ResponseCanId, 0x7E8);
        assert_eq!(address.m_addressingMode, CanAddressingMode::Normal11Bit);
        // Both identifiers were on the bus, so nothing was guessed.
        assert_eq!(address.m_confidence, Confidence::Observed);
    }

    #[test]
    fn interleaved_two_ecu_exchange_pairs_each_request_with_its_own_ecu() {
        // Both testers' requests are outstanding at the same time, and 0x7E1 is answered
        // first — a single "most recent request" rule would mis-pair these.
        let frames = vec![
            f(0x7E0, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x7E1, 0.002, vec![0x03, 0x22, 0xF1, 0x91]),
            f(0x7E9, 0.003, vec![0x04, 0x62, 0xF1, 0x91, 0x42]),
            f(0x7E8, 0.004, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 2);

        let first = &vehicle.m_vecEcus[0];
        let second = &vehicle.m_vecEcus[1];

        assert_eq!(first.m_optCanAddress.unwrap().m_u32RequestCanId, 0x7E0);
        assert_eq!(first.m_optCanAddress.unwrap().m_u32ResponseCanId, 0x7E8);
        assert_eq!(first.FindDid(0xF190).unwrap().m_vecValue, vec![0x41]);

        assert_eq!(second.m_optCanAddress.unwrap().m_u32RequestCanId, 0x7E1);
        assert_eq!(second.m_optCanAddress.unwrap().m_u32ResponseCanId, 0x7E9);
        assert_eq!(second.FindDid(0xF191).unwrap().m_vecValue, vec![0x42]);
    }

    #[test]
    fn functional_request_infers_each_ecus_own_request_id() {
        // One broadcast request on 0x7DF, answered by two ECUs on their own identifiers. The
        // physical request identifiers were never on the bus, so they are inferred.
        let frames = vec![
            f(0x7DF, 0.001, vec![0x02, 0x10, 0x03]),
            f(0x7E8, 0.002, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
            f(0x7DF, 0.003, vec![0x02, 0x10, 0x03]),
            f(0x7E9, 0.004, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 2);

        for (ecu, u32ExpectedRequestId) in vehicle.m_vecEcus.iter().zip([0x7E0, 0x7E1]) {
            let address = ecu.m_optCanAddress.expect("CAN address recorded");
            assert_eq!(address.m_u32RequestCanId, u32ExpectedRequestId);
            // 0x7DF is shared, so this ECU's own request identifier was never observed.
            assert_eq!(address.m_confidence, Confidence::Inferred);
        }
    }

    #[test]
    fn observed_address_is_not_downgraded_by_a_later_functional_exchange() {
        let frames = vec![
            f(0x7E0, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x7E8, 0.002, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
            f(0x7DF, 0.003, vec![0x02, 0x10, 0x03]),
            f(0x7E8, 0.004, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        let address = vehicle.m_vecEcus[0].m_optCanAddress.unwrap();
        assert_eq!(address.m_u32RequestCanId, 0x7E0);
        assert_eq!(address.m_confidence, Confidence::Observed);
    }

    #[test]
    fn reconstructs_29_bit_normal_fixed_addressing() {
        // Tester 0xF1 addressing ECU 0x10: request 0x18DA10F1, response 0x18DAF110.
        let frames = vec![
            f(0x18DA10F1, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x18DAF110, 0.002, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 1);

        let ecu = &vehicle.m_vecEcus[0];
        // The ECU's logical address is the source byte of the identifier it answers on.
        assert_eq!(ecu.m_u16LogicalAddress, 0x10);

        let address = ecu.m_optCanAddress.expect("CAN address recorded");
        assert_eq!(address.m_u32RequestCanId, 0x18DA10F1);
        assert_eq!(address.m_u32ResponseCanId, 0x18DAF110);
        assert_eq!(
            address.m_addressingMode,
            CanAddressingMode::NormalFixed29Bit
        );
        assert_eq!(address.m_confidence, Confidence::Observed);
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
