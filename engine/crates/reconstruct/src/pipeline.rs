//! Reconstruction pipeline: frames -> ISO-TP PDUs -> correlated UDS pairs -> Vehicle model.
//!
//! See ADR 0003 for the design. Every fact produced here is `Confidence::Observed` because it
//! was seen in a trace rather than taken from a specification.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use crate::behaviour::*;
use can::CanFrame;
use core_domain::model::{
    c_u32Functional11BitCanId, c_u32LegislatedRequestCanIdFirst, c_u32LegislatedRequestCanIdLast,
    c_u32Response11BitOffset, CanAddress, CanAddressingMode, DefaultFunctionalCanId, Ecu, Vehicle,
};
use core_domain::Confidence;
use isotp::ReassembleStream;

/// The N_TAtype byte (bits 23..16 of a 29-bit identifier) that marks normal-fixed physical
/// addressing; 0xDB marks the functional variant (ISO 15765-2).
const c_byNormalFixedPhysicalTaType: u8 = 0xDA;
const c_byNormalFixedFunctionalTaType: u8 = 0xDB;
/// Upper bound on outstanding unanswered requests kept during correlation. Logs routinely
/// contain requests nothing ever answers; without a cap the list would grow with the log.
const c_uMaxPendingRequests: usize = 64;

/// One reassembled PDU tagged with the CAN ID it arrived on and its time.
struct PduRecord {
    m_u32CanId: u32,
    /// When the PDU's first frame appeared.
    m_f64StartedAtSec: f64,
    /// When its last frame appeared. Equal to the start for a single-frame PDU.
    m_f64CompletedAtSec: f64,
    m_vecBytes: Vec<u8>,
    /// Whether the source log said this travelled from the tester to an ECU. `None` for a
    /// format that records no direction, where it has to be inferred from the service id.
    m_optBIsRequest: Option<bool>,
}

/// A request awaiting its response during correlation.
struct PendingRequest {
    /// CAN identifier the request was sent on (physical, e.g. 0x7E0, or functional 0x7DF).
    m_u32RequestCanId: u32,
    m_byServiceId: u8,
    m_vecBytes: Vec<u8>,
    /// When the last frame of the request appeared. Nothing that started transmitting before
    /// this instant can be an answer to it.
    m_f64CompletedAtSec: f64,
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
        if !IsRequest(pdu) && TryApplyAsResponse(pdu, &mut vecPending, &mut mapEcus) {
            continue;
        }

        if IsRequest(pdu) {
            RememberRequest(&mut vecPending, pdu);
        }
        // Unmatched responses are ignored.
    }

    Vehicle {
        m_strName: "Reconstructed Vehicle".to_string(),
        m_vecEcus: mapEcus.into_values().collect(),
        // No networks: a tester-side capture sees one connector and cannot tell whether these
        // ECUs share a wire or sit behind a gateway. Inventing a bus here would turn "we do
        // not know" into a claim.
        m_vecNetworks: Vec::new(),
        // A capture carries no vehicle identity: no VIN unless an ECU was asked for one, and
        // certainly no EID or GID. Left unprogrammed, which announces honestly.
        m_identity: Default::default(),
    }
}

/// True when this PDU is a request from the tester.
///
/// A log that records the direction of each frame is believed outright; otherwise the service
/// identifier decides, which is a good heuristic but only a heuristic.
fn IsRequest(pdu: &PduRecord) -> bool {
    match pdu.m_optBIsRequest {
        Some(bIsRequest) => bIsRequest,
        None => IsRequestSid(pdu.m_vecBytes[0]),
    }
}

/// Try to pair this PDU with an outstanding request and fold the exchange into the model.
/// Returns false when it answers nothing, so the caller can consider it as a new request.
fn TryApplyAsResponse(
    pdu: &PduRecord,
    vecPending: &mut Vec<PendingRequest>,
    mapEcus: &mut BTreeMap<u32, Ecu>,
) -> bool {
    let uIndex = match FindPendingRequestFor(vecPending, pdu) {
        Some(uIndex) => uIndex,
        None => return false,
    };

    // A functional request is answered by every ECU that listens on the broadcast identifier,
    // so it stays outstanding for the ECUs still to answer; a physical request has exactly one
    // answer and is retired once it arrives.
    let bIsFunctional = IsFunctionalRequestCanId(vecPending[uIndex].m_u32RequestCanId);
    let pending = if bIsFunctional {
        ClonePendingRequest(&vecPending[uIndex])
    } else {
        vecPending.remove(uIndex)
    };

    let ecu = EcuFor(mapEcus, pdu.m_u32CanId);
    RecordCanAddress(ecu, pending.m_u32RequestCanId, pdu.m_u32CanId);
    ApplyPair(ecu, &pending.m_vecBytes, &pdu.m_vecBytes);
    true
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
        // A diagnostic CAN identifier carries one direction: a tester's requests or an ECU's
        // responses, never both. So the direction of the stream's first frame describes every
        // PDU reassembled from it.
        let optBIsRequest = vecStream.first().and_then(|frame| frame.m_optBIsRequest);

        for msg in ReassembleStream(vecStream) {
            vecPdus.push(PduRecord {
                m_u32CanId: *u32CanId,
                m_f64StartedAtSec: msg.m_f64StartedAtSec,
                m_f64CompletedAtSec: msg.m_f64CompletedAtSec,
                m_vecBytes: msg.m_vecData,
                m_optBIsRequest: optBIsRequest,
            });
        }
    }

    // Global time order so request/response correlation follows the real exchange.
    //
    // Ordered by when each PDU *started*, which is the order a bus observer saw them begin. It
    // is deliberately not completion order: a long response can still be in flight when the
    // tester sends its next request, and completion order would let that later request evict
    // the one the response actually answers. What completion time is for is the rule in
    // `FindPendingRequestFor` — it constrains which pairings are physically possible.
    vecPdus.sort_by(|a, b| {
        a.m_f64StartedAtSec
            .partial_cmp(&b.m_f64StartedAtSec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    vecPdus
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
        m_f64CompletedAtSec: pdu.m_f64CompletedAtSec,
    });
}

/// Find which outstanding request `pdu` answers, if any.
///
/// Three rules, in order:
///   0. **Causality** — the response must have started transmitting after the request finished
///      arriving. An ECU cannot begin answering a request it has not yet fully received, so a
///      pairing that violates this is not merely unlikely, it is impossible. Without the check
///      a long request (a `0x2E` write, a `0x36` TransferData) would swallow whatever response
///      happened to arrive while it was still being sent — and reconstruction would record
///      that as observed behaviour.
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
        .filter(|(_, pending)| CouldHaveAnswered(pending, pdu))
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

/// Whether this PDU could physically be an answer to that request.
///
/// The response has to have started after the request finished. Timestamps in a capture are
/// not perfectly precise, so frames recorded at the very same instant are allowed — the check
/// exists to reject a response that began *while the request was still being transmitted*,
/// which is a gap of whole frames, not of rounding.
fn CouldHaveAnswered(pending: &PendingRequest, pdu: &PduRecord) -> bool {
    pdu.m_f64StartedAtSec >= pending.m_f64CompletedAtSec
}

/// The response identifier conventionally paired with a request identifier, or `None` when no
/// convention applies (e.g. the functional identifier 0x7DF, which every ECU answers on its
/// own physical identifier).
fn DeriveResponseCanId(u32RequestCanId: u32) -> Option<u32> {
    if IsFunctionalRequestCanId(u32RequestCanId) {
        return None;
    }
    if IsNormal11BitRequestId(u32RequestCanId) {
        return Some(u32RequestCanId + c_u32Response11BitOffset);
    }
    if SplitNormalFixed29BitId(u32RequestCanId).is_some() {
        // 29-bit normal fixed (ISO 15765-2): the answer swaps target and source.
        return Some(SwapTargetAndSource(u32RequestCanId));
    }
    None
}

/// The request identifier conventionally paired with a response identifier, or `None` when the
/// pairing cannot be derived.
fn DeriveRequestCanId(u32ResponseCanId: u32) -> Option<u32> {
    if IsNormal11BitResponseId(u32ResponseCanId) {
        return Some(u32ResponseCanId - c_u32Response11BitOffset);
    }
    if SplitNormalFixed29BitId(u32ResponseCanId).is_some() {
        return Some(SwapTargetAndSource(u32ResponseCanId));
    }
    None
}

/// True for the legislated 11-bit UDS request identifiers 0x7E0..=0x7E7 (ISO 15765-4). Outside
/// this range the request/response pairing is OEM-specific and cannot be derived.
fn IsNormal11BitRequestId(u32CanId: u32) -> bool {
    (c_u32LegislatedRequestCanIdFirst..=c_u32LegislatedRequestCanIdLast).contains(&u32CanId)
}

/// True for the legislated 11-bit UDS response identifiers 0x7E8..=0x7EF (ISO 15765-4).
fn IsNormal11BitResponseId(u32CanId: u32) -> bool {
    let u32First = c_u32LegislatedRequestCanIdFirst + c_u32Response11BitOffset;
    let u32Last = c_u32LegislatedRequestCanIdLast + c_u32Response11BitOffset;
    (u32First..=u32Last).contains(&u32CanId)
}

/// Split a 29-bit normal-fixed **physical** identifier (`<prio><res><DP>DA<target><source>`)
/// into its target and source addresses, or `None` if it is not one.
///
/// Only the N_TAtype byte is checked, not the whole high half: the priority bits are not fixed
/// by the standard (0x18 is common, 0x1C also occurs), so matching a hard-coded 0x18DA prefix
/// would miss valid identifiers.
fn SplitNormalFixed29BitId(u32CanId: u32) -> Option<(u8, u8)> {
    if NormalFixedTaTypeOf(u32CanId) != Some(c_byNormalFixedPhysicalTaType) {
        return None;
    }
    let byTarget = ((u32CanId >> 8) & 0xFF) as u8;
    let bySource = (u32CanId & 0xFF) as u8;
    Some((byTarget, bySource))
}

/// The N_TAtype byte of a 29-bit identifier (0xDA physical, 0xDB functional), or `None` for an
/// 11-bit identifier.
fn NormalFixedTaTypeOf(u32CanId: u32) -> Option<u8> {
    if u32CanId <= 0x7FF {
        return None;
    }
    Some(((u32CanId >> 16) & 0xFF) as u8)
}

/// Swap the target and source addresses of a 29-bit normal-fixed identifier, preserving the
/// priority, reserved, data-page and N_TAtype bits: 0x18DAD4F1 -> 0x18DAF1D4.
fn SwapTargetAndSource(u32CanId: u32) -> u32 {
    let u32Header = u32CanId & 0xFFFF_0000;
    let u32Target = (u32CanId >> 8) & 0xFF;
    let u32Source = u32CanId & 0xFF;
    u32Header | (u32Source << 8) | u32Target
}

/// True for a functional (broadcast) request identifier: the legislated 11-bit 0x7DF, or a
/// 29-bit identifier whose N_TAtype marks it functional.
fn IsFunctionalRequestCanId(u32CanId: u32) -> bool {
    if u32CanId == c_u32Functional11BitCanId {
        return true;
    }
    NormalFixedTaTypeOf(u32CanId) == Some(c_byNormalFixedFunctionalTaType)
}

/// Copy an outstanding request so it can be matched again by another responder.
fn ClonePendingRequest(pending: &PendingRequest) -> PendingRequest {
    PendingRequest {
        m_u32RequestCanId: pending.m_u32RequestCanId,
        m_byServiceId: pending.m_byServiceId,
        m_vecBytes: pending.m_vecBytes.clone(),
        m_f64CompletedAtSec: pending.m_f64CompletedAtSec,
    }
}

/// Record the CAN identifiers an ECU is reached on, from one correlated exchange.
///
/// A physically addressed exchange puts both identifiers on the bus, so the pair is
/// `Observed`. A functionally addressed one only reveals the broadcast identifier the ECU
/// listens on; the ECU's own request identifier has to be derived from its response identifier
/// by an addressing convention, which is `Inferred` — and only possible at all for the
/// legislated 11-bit range or 29-bit normal fixed addressing.
fn RecordCanAddress(ecu: &mut Ecu, u32RequestCanId: u32, u32ResponseCanId: u32) {
    if IsFunctionalRequestCanId(u32RequestCanId) {
        RecordFunctionalListen(ecu, u32RequestCanId, u32ResponseCanId);
        return;
    }

    let mode = AddressingModeOf(u32RequestCanId, u32ResponseCanId);
    let optU32FunctionalCanId =
        ExistingFunctionalCanId(ecu).or_else(|| DefaultFunctionalCanId(u32RequestCanId, mode));

    ecu.m_optCanAddress = Some(CanAddress {
        m_u32RequestCanId: u32RequestCanId,
        m_u32ResponseCanId: u32ResponseCanId,
        m_optU32FunctionalCanId: optU32FunctionalCanId,
        m_addressingMode: mode,
        m_confidence: Confidence::Observed,
    });
}

/// Record that an ECU answered a broadcast request.
///
/// If the ECU's physical addressing is already known, only the broadcast identifier is added —
/// an observed physical pair is never downgraded by an inference. If this broadcast is the
/// first sighting of the ECU, its own request identifier is derived where a convention allows
/// and marked `Inferred`; where none does (an OEM identifier pair), the ECU is left without a
/// CAN address rather than having the shared broadcast identifier recorded as its own, which
/// would collide with every other listener.
fn RecordFunctionalListen(ecu: &mut Ecu, u32FunctionalCanId: u32, u32ResponseCanId: u32) {
    if let Some(address) = ecu.m_optCanAddress.as_mut() {
        address.m_optU32FunctionalCanId = Some(u32FunctionalCanId);
        return;
    }

    let u32RequestCanId = match DeriveRequestCanId(u32ResponseCanId) {
        Some(u32Derived) => u32Derived,
        None => {
            tracing::debug!(
                responseCanId = format!("{u32ResponseCanId:03X}"),
                "ECU answered a broadcast on an identifier no convention pairs; \
                 leaving it unaddressed until a physical exchange is seen"
            );
            return;
        }
    };

    ecu.m_optCanAddress = Some(CanAddress {
        m_u32RequestCanId: u32RequestCanId,
        m_u32ResponseCanId: u32ResponseCanId,
        m_optU32FunctionalCanId: Some(u32FunctionalCanId),
        m_addressingMode: AddressingModeOf(u32RequestCanId, u32ResponseCanId),
        m_confidence: Confidence::Inferred,
    });
}

/// The functional identifier already recorded for this ECU, if any.
fn ExistingFunctionalCanId(ecu: &Ecu) -> Option<u32> {
    ecu.m_optCanAddress
        .and_then(|address| address.m_optU32FunctionalCanId)
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

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::model::SessionType;

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
    fn a_response_that_began_before_its_request_finished_is_not_paired_with_it() {
        // An ECU cannot start answering a request it has not finished receiving. Here a long
        // request is still being transmitted at t=2.0 when a response appears — that response
        // answers something else, and pairing it here would record DID 0xF190 as behaviour
        // this ECU was observed to have.
        let frames = vec![
            // Request on 0x7E0, 13 bytes: reads five DIDs. Starts at 1.0, finishes at 3.0.
            f(
                0x7E0,
                1.0,
                vec![0x10, 0x0D, 0x22, 0xF1, 0x90, 0xF1, 0x8C, 0xF1],
            ),
            f(
                0x7E0,
                3.0,
                vec![0x21, 0x91, 0xF1, 0x92, 0xF1, 0x93, 0xF1, 0x94],
            ),
            // A response on the conventional partner identifier, mid-request.
            f(0x7E8, 2.0, vec![0x06, 0x62, 0xF1, 0x90, 0x01, 0x02, 0x03]),
            // A second, entirely ordinary exchange with another ECU, to prove the causality
            // rule rejects the impossible pairing rather than breaking correlation outright.
            f(
                0x7E1,
                4.0,
                vec![0x03, 0x22, 0xF1, 0x8C, 0x00, 0x00, 0x00, 0x00],
            ),
            f(0x7E9, 4.1, vec![0x06, 0x62, 0xF1, 0x8C, 0x41, 0x42, 0x43]),
        ];

        let vehicle = ReconstructFromFrames(&frames);

        assert_eq!(
            vehicle.m_vecEcus.len(),
            1,
            "only the well-formed exchange should have produced an ECU"
        );
        assert_eq!(vehicle.m_vecEcus[0].m_u16LogicalAddress, 0x7E9);
        assert!(
            vehicle.m_vecEcus[0].m_mapDids.contains_key(&0xF18C),
            "the valid exchange must still be recorded"
        );
        assert!(
            !vehicle
                .m_vecEcus
                .iter()
                .any(|ecu| ecu.m_mapDids.contains_key(&0xF190)),
            "DID 0xF190 was never answered; nothing should claim it was"
        );
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
    fn ordinary_periodic_traffic_does_not_disturb_correlation() {
        // A powertrain frame whose first byte happens to look like a valid single-frame PCI
        // sits between a real request and its real response. Real logs are mostly traffic
        // like this, so it must not be mistaken for a diagnostic request.
        let frames = vec![
            f(0x7E0, 0.0010, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x0C9, 0.0015, vec![0x02, 0x12, 0x34, 0x00]),
            f(0x7E8, 0.0020, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
        ];

        let vehicle = ReconstructFromFrames(&frames);

        assert_eq!(
            vehicle.m_vecEcus.len(),
            1,
            "only the diagnostic ECU is discovered"
        );
        let ecu = &vehicle.m_vecEcus[0];
        assert_eq!(ecu.m_optCanAddress.unwrap().m_u32RequestCanId, 0x7E0);
        assert_eq!(ecu.FindDid(0xF190).unwrap().m_vecValue, vec![0x41]);
    }

    #[test]
    fn discovers_a_service_from_the_high_request_sid_range() {
        // 0x85 ControlDTCSetting / 0xC5 positive response — outside the 0x10..=0x3E range but
        // a normal part of a flashing sequence.
        let frames = vec![
            f(0x7E0, 0.001, vec![0x02, 0x85, 0x02]),
            f(0x7E8, 0.002, vec![0x02, 0xC5, 0x02]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 1);
        assert!(vehicle.m_vecEcus[0].m_vecSupportedServices.contains(&0x85));
    }

    #[test]
    fn a_legislated_pair_listens_functionally_but_an_oem_pair_does_not() {
        let frames = vec![
            f(0x7E0, 0.001, vec![0x02, 0x10, 0x03]),
            f(0x7E8, 0.002, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
            // 0x745 -> 0x765 is a real OEM pair: +0x20, outside anything a standard defines.
            f(0x745, 0.003, vec![0x02, 0x10, 0x03]),
            f(0x765, 0.004, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 2);

        let mapByRequestId: Vec<(u32, Option<u32>)> = vehicle
            .m_vecEcus
            .iter()
            .map(|ecu| {
                let address = ecu.m_optCanAddress.expect("CAN address recorded");
                (address.m_u32RequestCanId, address.m_optU32FunctionalCanId)
            })
            .collect();

        // ISO 15765-4 mandates 0x7DF only for the legislated range; the OEM pair gets nothing.
        assert!(mapByRequestId.contains(&(0x7E0, Some(c_u32Functional11BitCanId))));
        assert!(mapByRequestId.contains(&(0x745, None)));
    }

    #[test]
    fn a_29_bit_ecu_listens_on_the_normal_fixed_functional_id() {
        let frames = vec![
            f(0x18DAD4F1, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x18DAF1D4, 0.002, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        let address = vehicle.m_vecEcus[0].m_optCanAddress.unwrap();
        assert_eq!(address.m_optU32FunctionalCanId, Some(0x18DB33F1));
        assert!(address.IsExtendedId());
    }

    #[test]
    fn a_29_bit_priority_other_than_0x18_is_still_recognised() {
        // The priority bits are not fixed by ISO 15765-2; only the N_TAtype byte (0xDA) marks
        // normal-fixed physical addressing.
        let frames = vec![
            f(0x1CDAD4F1, 0.001, vec![0x03, 0x22, 0xF1, 0x90]),
            f(0x1CDAF1D4, 0.002, vec![0x04, 0x62, 0xF1, 0x90, 0x41]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        let address = vehicle.m_vecEcus[0].m_optCanAddress.unwrap();
        assert_eq!(address.m_u32RequestCanId, 0x1CDAD4F1);
        assert_eq!(
            address.m_addressingMode,
            CanAddressingMode::NormalFixed29Bit
        );
    }

    #[test]
    fn one_broadcast_request_discovers_every_ecu_that_answers_it() {
        // A tester discovers the bus by broadcasting: one request, several answers. All of
        // them must be recorded, not just the first.
        let frames = vec![
            f(0x7DF, 0.001, vec![0x02, 0x3E, 0x00]),
            f(0x7E8, 0.002, vec![0x02, 0x7E, 0x00]),
            f(0x7E9, 0.003, vec![0x02, 0x7E, 0x00]),
            f(0x7EA, 0.004, vec![0x02, 0x7E, 0x00]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 3);

        for ecu in &vehicle.m_vecEcus {
            let address = ecu.m_optCanAddress.expect("CAN address recorded");
            assert_eq!(
                address.m_optU32FunctionalCanId,
                Some(c_u32Functional11BitCanId)
            );
            assert_eq!(address.m_confidence, Confidence::Inferred);
        }
    }

    #[test]
    fn an_oem_ecu_seen_only_on_a_broadcast_is_left_unaddressed() {
        // 0x765 is an OEM response identifier; no convention says which identifier a tester
        // reaches it on, and recording the shared 0x7DF as its own would collide with every
        // other listener.
        let frames = vec![
            f(0x7DF, 0.001, vec![0x02, 0x3E, 0x00]),
            f(0x765, 0.002, vec![0x02, 0x7E, 0x00]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        assert_eq!(vehicle.m_vecEcus.len(), 1);

        let ecu = &vehicle.m_vecEcus[0];
        assert!(ecu.m_vecSupportedServices.contains(&0x3E));
        assert!(
            ecu.m_optCanAddress.is_none(),
            "no identifier pair can be derived, so none is invented"
        );
    }

    #[test]
    fn a_later_broadcast_adds_the_functional_id_to_an_observed_pair() {
        let frames = vec![
            f(0x745, 0.001, vec![0x02, 0x10, 0x03]),
            f(0x765, 0.002, vec![0x06, 0x50, 0x03, 0x00, 0x32, 0x01, 0xF4]),
            f(0x7DF, 0.003, vec![0x02, 0x3E, 0x00]),
            f(0x765, 0.004, vec![0x02, 0x7E, 0x00]),
        ];

        let vehicle = ReconstructFromFrames(&frames);
        let address = vehicle.m_vecEcus[0]
            .m_optCanAddress
            .expect("CAN address recorded");

        assert_eq!(address.m_u32RequestCanId, 0x745);
        assert_eq!(address.m_confidence, Confidence::Observed);
        // The broadcast identifier was observed even though no standard mandates it here.
        assert_eq!(
            address.m_optU32FunctionalCanId,
            Some(c_u32Functional11BitCanId)
        );
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
