//! UDS (ISO 14229) request handling — the pure protocol logic.
//!
//! This module is deliberately transport-agnostic and side-effect free: it takes the request
//! bytes plus a snapshot of the ECU's diagnostic state and returns the response bytes plus
//! the state changes the engine should apply. Keeping every UDS decision explicit here (one
//! function per service, explicit positive/negative branches) is intentional — see the
//! diagnostic-flow guidance in CLAUDE.md. The FFI wrapper in `lib.rs` only marshals types.

#![allow(non_snake_case, non_upper_case_globals)]

use plugin_contract::protocol::{
    c_byStateChangeEndTransfer, c_byStateChangeResetToDefaultSession,
    c_byStateChangeSetActiveSeedLevel, c_byStateChangeSetBlockSequenceCounter,
    c_byStateChangeSetSession, c_byStateChangeUnlockSecurity, REcuSnapshot, RKeyPolicy,
    RStateChange,
};

// --- Request Service IDs (SIDs) ---------------------------------------------------------
/// DiagnosticSessionControl.
pub const c_bySidDiagnosticSessionControl: u8 = 0x10;
/// ECUReset.
pub const c_bySidEcuReset: u8 = 0x11;
/// ReadDTCInformation.
pub const c_bySidReadDtcInformation: u8 = 0x19;
/// ReadDataByIdentifier.
pub const c_bySidReadDataByIdentifier: u8 = 0x22;
/// SecurityAccess.
pub const c_bySidSecurityAccess: u8 = 0x27;
/// RequestDownload.
pub const c_bySidRequestDownload: u8 = 0x34;
/// RequestUpload.
pub const c_bySidRequestUpload: u8 = 0x35;
/// TransferData.
pub const c_bySidTransferData: u8 = 0x36;
/// RequestTransferExit.
pub const c_bySidRequestTransferExit: u8 = 0x37;

/// The first block sequence counter of any transfer (ISO 14229-1 clause 14.2.4).
pub const c_byFirstBlockSequenceCounter: u8 = 0x01;
/// `lengthFormatIdentifier` in the RequestDownload response: the maximum block length is
/// announced in two bytes.
pub const c_byLengthFormatIdentifier: u8 = 0x20;
/// The largest block this server accepts, including the TransferData header. Modest on purpose:
/// a simulator has no memory to size it against, and an invented number is one a tester trusts.
pub const c_u16MaxBlockLength: u16 = 0x0400;
/// RoutineControl.
pub const c_bySidRoutineControl: u8 = 0x31;
/// TesterPresent.
pub const c_bySidTesterPresent: u8 = 0x3E;

/// Offset added to a request SID to form the positive-response SID (ISO 14229).
const c_byPositiveResponseOffset: u8 = 0x40;
/// Marker byte that starts every negative response.
const c_byNegativeResponseSid: u8 = 0x7F;
/// Bit in a sub-function byte that requests suppression of a positive response.
const c_bySuppressPositiveResponseBit: u8 = 0x80;

// --- Negative Response Codes (NRCs) -----------------------------------------------------
/// serviceNotSupported.
pub const c_byNrcServiceNotSupported: u8 = 0x11;
/// subFunctionNotSupported.
pub const c_byNrcSubFunctionNotSupported: u8 = 0x12;
/// The block sequence counter was neither the expected one nor a repeat of the previous.
pub const c_byNrcWrongBlockSequenceCounter: u8 = 0x73;
/// incorrectMessageLengthOrInvalidFormat.
pub const c_byNrcIncorrectMessageLength: u8 = 0x13;
/// conditionsNotCorrect.
pub const c_byNrcConditionsNotCorrect: u8 = 0x22;
/// requestSequenceError.
pub const c_byNrcRequestSequenceError: u8 = 0x24;
/// requestOutOfRange.
pub const c_byNrcRequestOutOfRange: u8 = 0x31;
/// invalidKey.
pub const c_byNrcInvalidKey: u8 = 0x35;
/// serviceNotSupportedInActiveSession.
pub const c_byNrcServiceNotSupportedInActiveSession: u8 = 0x7F;

/// UDS default session sub-function (0x01). Kept local so the handler needs no `core-domain`.
const c_bySessionDefault: u8 = 0x01;
/// Extended session sub-function (0x03).
const c_bySessionExtended: u8 = 0x03;
/// Programming session sub-function (0x02).
const c_bySessionProgramming: u8 = 0x02;

/// The result of handling one UDS request: the response bytes plus the state changes to
/// apply. An empty `m_vecResponse` means "suppress the positive response".
pub struct UdsReply {
    pub m_vecResponse: Vec<u8>,
    pub m_vecChanges: Vec<RStateChange>,
}

impl UdsReply {
    /// A reply with a response and no state changes.
    fn ResponseOnly(vecResponse: Vec<u8>) -> Self {
        UdsReply {
            m_vecResponse: vecResponse,
            m_vecChanges: Vec::new(),
        }
    }

    /// A negative response `7F <sid> <nrc>` with no state changes.
    fn Negative(byServiceId: u8, byNrc: u8) -> Self {
        UdsReply::ResponseOnly(vec![c_byNegativeResponseSid, byServiceId, byNrc])
    }
}

/// Build a state-change record.
fn MakeStateChange(byKind: u8, byValue: u8) -> RStateChange {
    RStateChange {
        m_byKind: byKind,
        m_byValue: byValue,
    }
}

/// Top-level dispatch: validate the request, check the service is supported, then route to
/// the per-service handler.
pub fn HandleRequest(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    if vecRequest.is_empty() {
        // Nothing to act on; report an invalid-format negative response against SID 0x00.
        return UdsReply::Negative(0x00, c_byNrcIncorrectMessageLength);
    }

    let byServiceId = vecRequest[0];

    // Permissive mode skips the supported-service list. The list describes what the ECU was
    // observed or declared to offer, and in permissive mode the operator has said that is not
    // the authority — a handler below, or a response override above, is.
    let bIsDeclared = snapshot.m_vecSupportedServices.contains(&byServiceId);
    if !bIsDeclared && !snapshot.m_bIsPermissive {
        return UdsReply::Negative(byServiceId, c_byNrcServiceNotSupported);
    }

    match byServiceId {
        c_bySidDiagnosticSessionControl => HandleDiagnosticSessionControl(vecRequest, snapshot),
        c_bySidEcuReset => HandleEcuReset(vecRequest),
        c_bySidReadDataByIdentifier => HandleReadDataByIdentifier(vecRequest, snapshot),
        c_bySidSecurityAccess => HandleSecurityAccess(vecRequest, snapshot),
        c_bySidReadDtcInformation => HandleReadDtcInformation(vecRequest, snapshot),
        c_bySidRoutineControl => HandleRoutineControl(vecRequest),
        c_bySidTesterPresent => HandleTesterPresent(vecRequest),
        c_bySidRequestDownload | c_bySidRequestUpload => HandleRequestTransfer(vecRequest),
        c_bySidTransferData => HandleTransferData(vecRequest, snapshot),
        c_bySidRequestTransferExit => HandleRequestTransferExit(),
        _ => UdsReply::Negative(byServiceId, c_byNrcServiceNotSupported),
    }
}

/// 0x34 RequestDownload / 0x35 RequestUpload — accept the transfer and arm the block counter.
///
/// The block length announced is deliberately modest and fixed. A simulator has no memory to
/// size it against, and a number invented to look plausible is a number a tester will believe.
fn HandleRequestTransfer(vecRequest: &[u8]) -> UdsReply {
    // SID + dataFormatIdentifier + addressAndLengthFormatIdentifier, then the address and size
    // those two describe. Anything shorter cannot be read at all.
    if vecRequest.len() < 3 {
        return UdsReply::Negative(vecRequest[0], c_byNrcIncorrectMessageLength);
    }

    UdsReply {
        m_vecResponse: vec![
            vecRequest[0] + c_byPositiveResponseOffset,
            c_byLengthFormatIdentifier,
            (c_u16MaxBlockLength >> 8) as u8,
            (c_u16MaxBlockLength & 0xFF) as u8,
        ],
        // ISO 14229-1 clause 14.2.4: the first TransferData of a transfer carries 0x01.
        m_vecChanges: vec![MakeStateChange(
            c_byStateChangeSetBlockSequenceCounter,
            c_byFirstBlockSequenceCounter,
        )],
    }
}

/// 0x36 TransferData — echo the block sequence counter and advance it.
///
/// The counter is why this cannot be a response override: it changes with every block. It
/// starts at 0x01 after RequestDownload or RequestUpload, counts up, and wraps from 0xFF to
/// 0x00 rather than back to 0x01 — the wrap value implementations most often get wrong.
///
/// A block repeating the *previous* counter is a retransmission, not an error: ISO 14229-1
/// clause 14.2.5 has the server answer it exactly as it answered the first time, without
/// advancing, because the tester is telling us its own response went missing.
fn HandleTransferData(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    if vecRequest.len() < 2 {
        return UdsReply::Negative(c_bySidTransferData, c_byNrcIncorrectMessageLength);
    }

    if !snapshot.m_bIsTransferInProgress {
        // No RequestDownload or RequestUpload came first, so there is no transfer to add to.
        return UdsReply::Negative(c_bySidTransferData, c_byNrcRequestSequenceError);
    }

    let byExpected = snapshot.m_byExpectedBlockSequenceCounter;
    let byReceived = vecRequest[1];
    if byReceived == PreviousBlockSequenceCounter(byExpected) {
        return UdsReply::ResponseOnly(vec![
            c_bySidTransferData + c_byPositiveResponseOffset,
            byReceived,
        ]);
    }

    if byReceived != byExpected {
        return UdsReply::Negative(c_bySidTransferData, c_byNrcWrongBlockSequenceCounter);
    }

    UdsReply {
        m_vecResponse: vec![c_bySidTransferData + c_byPositiveResponseOffset, byReceived],
        m_vecChanges: vec![MakeStateChange(
            c_byStateChangeSetBlockSequenceCounter,
            NextBlockSequenceCounter(byReceived),
        )],
    }
}

/// 0x37 RequestTransferExit — end the transfer and disarm the counter.
fn HandleRequestTransferExit() -> UdsReply {
    UdsReply {
        m_vecResponse: vec![c_bySidRequestTransferExit + c_byPositiveResponseOffset],
        m_vecChanges: vec![MakeStateChange(c_byStateChangeEndTransfer, 0)],
    }
}

/// The counter after this one. ISO 14229-1 clause 14.2.4: 0xFF is followed by 0x00, not 0x01 —
/// only the *first* block of a transfer is 0x01.
fn NextBlockSequenceCounter(byCounter: u8) -> u8 {
    byCounter.wrapping_add(1)
}

/// The counter before this one, for recognising a retransmission.
fn PreviousBlockSequenceCounter(byCounter: u8) -> u8 {
    byCounter.wrapping_sub(1)
}

/// Split a sub-function byte into (suppressPositiveResponse, rawSubFunction).
fn SplitSubFunction(bySubFunction: u8) -> (bool, u8) {
    let bSuppress = (bySubFunction & c_bySuppressPositiveResponseBit) != 0;
    let byValue = bySubFunction & !c_bySuppressPositiveResponseBit;
    (bSuppress, byValue)
}

/// 0x10 DiagnosticSessionControl — change the active session if it is supported.
fn HandleDiagnosticSessionControl(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    if vecRequest.len() != 2 {
        return UdsReply::Negative(
            c_bySidDiagnosticSessionControl,
            c_byNrcIncorrectMessageLength,
        );
    }

    let (bSuppress, bySession) = SplitSubFunction(vecRequest[1]);

    let bIsSupported = snapshot.m_vecSupportedSessions.contains(&bySession);
    if !bIsSupported {
        return UdsReply::Negative(
            c_bySidDiagnosticSessionControl,
            c_byNrcSubFunctionNotSupported,
        );
    }

    let vecChanges = vec![MakeStateChange(c_byStateChangeSetSession, bySession)];

    if bSuppress {
        return UdsReply {
            m_vecResponse: Vec::new(),
            m_vecChanges: vecChanges,
        };
    }

    // Positive response echoes the session and the P2/P2* timing record (fixed for Phase 1:
    // P2 = 50 ms, P2* = 5000 ms encoded in 10 ms units = 500 = 0x01F4).
    let vecResponse = vec![
        c_bySidDiagnosticSessionControl + c_byPositiveResponseOffset,
        bySession,
        0x00,
        0x32,
        0x01,
        0xF4,
    ];
    UdsReply {
        m_vecResponse: vecResponse,
        m_vecChanges: vecChanges,
    }
}

/// 0x11 ECUReset — a reset returns the ECU to the default session and (implicitly) relocks
/// security. Phase 1 models the diagnostic effect only, not an actual power cycle.
fn HandleEcuReset(vecRequest: &[u8]) -> UdsReply {
    if vecRequest.len() != 2 {
        return UdsReply::Negative(c_bySidEcuReset, c_byNrcIncorrectMessageLength);
    }

    let (bSuppress, byResetType) = SplitSubFunction(vecRequest[1]);

    // Resetting clears any unlocked security and returns to the default session.
    let vecChanges = vec![
        MakeStateChange(c_byStateChangeResetToDefaultSession, 0x00),
        MakeStateChange(c_byStateChangeUnlockSecurity, 0x00),
        MakeStateChange(c_byStateChangeSetActiveSeedLevel, 0x00),
    ];

    if bSuppress {
        return UdsReply {
            m_vecResponse: Vec::new(),
            m_vecChanges: vecChanges,
        };
    }

    let vecResponse = vec![c_bySidEcuReset + c_byPositiveResponseOffset, byResetType];
    UdsReply {
        m_vecResponse: vecResponse,
        m_vecChanges: vecChanges,
    }
}

/// 0x22 ReadDataByIdentifier — return the configured value for a known DID.
fn HandleReadDataByIdentifier(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    if vecRequest.len() != 3 {
        return UdsReply::Negative(c_bySidReadDataByIdentifier, c_byNrcIncorrectMessageLength);
    }

    let u16Did = ((vecRequest[1] as u16) << 8) | (vecRequest[2] as u16);

    let optDid = snapshot.m_vecDids.iter().find(|did| did.m_u16Id == u16Did);
    let did = match optDid {
        Some(did) => did,
        None => {
            return UdsReply::Negative(c_bySidReadDataByIdentifier, c_byNrcRequestOutOfRange);
        }
    };

    let mut vecResponse = Vec::with_capacity(3 + did.m_vecValue.len());
    vecResponse.push(c_bySidReadDataByIdentifier + c_byPositiveResponseOffset);
    vecResponse.push(vecRequest[1]);
    vecResponse.push(vecRequest[2]);
    vecResponse.extend_from_slice(did.m_vecValue.as_slice());

    UdsReply::ResponseOnly(vecResponse)
}

/// 0x27 SecurityAccess — requestSeed (odd sub-function) then sendKey (even sub-function).
/// Requires a non-default session, mirroring typical ECU behaviour.
fn HandleSecurityAccess(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    if vecRequest.len() < 2 {
        return UdsReply::Negative(c_bySidSecurityAccess, c_byNrcIncorrectMessageLength);
    }

    if snapshot.m_byCurrentSession == c_bySessionDefault {
        return UdsReply::Negative(
            c_bySidSecurityAccess,
            c_byNrcServiceNotSupportedInActiveSession,
        );
    }

    let bySubFunction = vecRequest[1];
    let bIsRequestSeed = (bySubFunction & 0x01) == 0x01;

    if bIsRequestSeed {
        HandleSecurityRequestSeed(bySubFunction, snapshot)
    } else {
        HandleSecuritySendKey(vecRequest, bySubFunction, snapshot)
    }
}

/// requestSeed: return the level's seed, or an all-zero seed if already unlocked.
fn HandleSecurityRequestSeed(bySubFunction: u8, snapshot: &REcuSnapshot) -> UdsReply {
    let optLevel = snapshot
        .m_vecSecurityLevels
        .iter()
        .find(|level| level.m_byRequestSeedSubFunction == bySubFunction);

    let level = match optLevel {
        Some(level) => level,
        None => {
            return UdsReply::Negative(c_bySidSecurityAccess, c_byNrcSubFunctionNotSupported);
        }
    };

    let mut vecResponse = vec![
        c_bySidSecurityAccess + c_byPositiveResponseOffset,
        bySubFunction,
    ];

    if snapshot.m_bySecurityUnlockedLevel == bySubFunction {
        // Already unlocked: ISO 14229 says return a zero seed of the same length.
        vecResponse.extend(std::iter::repeat_n(0x00u8, level.m_vecSeed.len()));
        return UdsReply::ResponseOnly(vecResponse);
    }

    vecResponse.extend_from_slice(level.m_vecSeed.as_slice());

    // Record which level has an outstanding seed so the paired sendKey is valid.
    UdsReply {
        m_vecResponse: vecResponse,
        m_vecChanges: vec![MakeStateChange(
            c_byStateChangeSetActiveSeedLevel,
            bySubFunction,
        )],
    }
}

/// sendKey: compare the supplied key against the expected key and unlock on match.
fn HandleSecuritySendKey(
    vecRequest: &[u8],
    bySubFunction: u8,
    snapshot: &REcuSnapshot,
) -> UdsReply {
    let optLevel = snapshot
        .m_vecSecurityLevels
        .iter()
        .find(|level| level.m_byRequestSeedSubFunction.wrapping_add(1) == bySubFunction);

    let level = match optLevel {
        Some(level) => level,
        None => {
            return UdsReply::Negative(c_bySidSecurityAccess, c_byNrcSubFunctionNotSupported);
        }
    };

    // A key may only be sent after its seed was requested. Checked before the policy, and
    // deliberately: ISO 14229-1 makes this a sequence rule, not a key rule, so even a level
    // that accepts any key still refuses one that arrives out of order.
    if snapshot.m_byActiveSeedLevel != level.m_byRequestSeedSubFunction {
        return UdsReply::Negative(c_bySidSecurityAccess, c_byNrcRequestSequenceError);
    }

    let vecProvidedKey = &vecRequest[2..];
    match level.m_keyPolicy {
        // A level whose key nobody has. Reconstruction produces these: a capture yields the
        // seed but never a key that will answer the *next* seed, so comparing would refuse
        // every tester.
        RKeyPolicy::AcceptAnyKey => {}

        // Fault injection: the tester's rejected-key path, on demand.
        RKeyPolicy::RefuseWith => {
            return UdsReply::Negative(c_bySidSecurityAccess, level.m_byRefusalNrc);
        }

        RKeyPolicy::CompareWithExpectedKey => {
            if vecProvidedKey != level.m_vecExpectedKey.as_slice() {
                return UdsReply::Negative(c_bySidSecurityAccess, c_byNrcInvalidKey);
            }
        }
    }

    // Key accepted: unlock this level and clear the outstanding seed.
    UdsReply {
        m_vecResponse: vec![
            c_bySidSecurityAccess + c_byPositiveResponseOffset,
            bySubFunction,
        ],
        m_vecChanges: vec![
            MakeStateChange(
                c_byStateChangeUnlockSecurity,
                level.m_byRequestSeedSubFunction,
            ),
            MakeStateChange(c_byStateChangeSetActiveSeedLevel, 0x00),
        ],
    }
}

/// 0x19 ReadDTCInformation — Phase 1 supports sub-function 0x02 (reportDTCByStatusMask).
fn HandleReadDtcInformation(vecRequest: &[u8], snapshot: &REcuSnapshot) -> UdsReply {
    const c_bySubReportByStatusMask: u8 = 0x02;

    if vecRequest.len() != 3 {
        return UdsReply::Negative(c_bySidReadDtcInformation, c_byNrcIncorrectMessageLength);
    }

    let bySubFunction = vecRequest[1];
    if bySubFunction != c_bySubReportByStatusMask {
        return UdsReply::Negative(c_bySidReadDtcInformation, c_byNrcSubFunctionNotSupported);
    }

    let byStatusMask = vecRequest[2];

    let mut vecResponse = vec![
        c_bySidReadDtcInformation + c_byPositiveResponseOffset,
        bySubFunction,
        0xFF, // statusAvailabilityMask — all bits available in this simple model
    ];

    for dtc in snapshot.m_vecDtcs.iter() {
        // Only report DTCs whose status intersects the requested mask.
        if (dtc.m_byStatus & byStatusMask) == 0 {
            continue;
        }
        vecResponse.push(((dtc.m_u32Code >> 16) & 0xFF) as u8);
        vecResponse.push(((dtc.m_u32Code >> 8) & 0xFF) as u8);
        vecResponse.push((dtc.m_u32Code & 0xFF) as u8);
        vecResponse.push(dtc.m_byStatus);
    }

    UdsReply::ResponseOnly(vecResponse)
}

/// 0x31 RoutineControl — Phase 1 acknowledges start/stop/requestResults for any routine id.
/// Actual routine execution arrives with the reconstruction phases.
fn HandleRoutineControl(vecRequest: &[u8]) -> UdsReply {
    const c_bySubStart: u8 = 0x01;
    const c_bySubStop: u8 = 0x02;
    const c_bySubRequestResults: u8 = 0x03;

    if vecRequest.len() < 4 {
        return UdsReply::Negative(c_bySidRoutineControl, c_byNrcIncorrectMessageLength);
    }

    let byRoutineControlType = vecRequest[1];
    let bIsKnownSubFunction = matches!(
        byRoutineControlType,
        c_bySubStart | c_bySubStop | c_bySubRequestResults
    );
    if !bIsKnownSubFunction {
        return UdsReply::Negative(c_bySidRoutineControl, c_byNrcSubFunctionNotSupported);
    }

    // Echo the routine identifier back in the positive response.
    let vecResponse = vec![
        c_bySidRoutineControl + c_byPositiveResponseOffset,
        byRoutineControlType,
        vecRequest[2],
        vecRequest[3],
    ];
    UdsReply::ResponseOnly(vecResponse)
}

/// 0x3E TesterPresent — a keep-alive; only sub-function 0x00 is defined.
fn HandleTesterPresent(vecRequest: &[u8]) -> UdsReply {
    if vecRequest.len() != 2 {
        return UdsReply::Negative(c_bySidTesterPresent, c_byNrcIncorrectMessageLength);
    }

    let (bSuppress, bySubFunction) = SplitSubFunction(vecRequest[1]);
    if bySubFunction != 0x00 {
        return UdsReply::Negative(c_bySidTesterPresent, c_byNrcSubFunctionNotSupported);
    }

    if bSuppress {
        return UdsReply::ResponseOnly(Vec::new());
    }

    UdsReply::ResponseOnly(vec![
        c_bySidTesterPresent + c_byPositiveResponseOffset,
        0x00,
    ])
}

// Silence "unused" for session constants referenced only in specific branches/tests.
#[allow(dead_code)]
const _: (u8, u8) = (c_bySessionExtended, c_bySessionProgramming);

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::std_types::RVec;
    use plugin_contract::protocol::{RDataIdentifier, RDtc, RSecurityLevel};

    /// Build a snapshot for a typical Phase-1 ECU used across tests.
    fn MakeSnapshot(
        byCurrentSession: u8,
        bySecurityUnlockedLevel: u8,
        byActiveSeedLevel: u8,
    ) -> REcuSnapshot {
        REcuSnapshot {
            m_byCurrentSession: byCurrentSession,
            m_bySecurityUnlockedLevel: bySecurityUnlockedLevel,
            m_byActiveSeedLevel: byActiveSeedLevel,
            m_vecSupportedServices: RVec::from(vec![0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x3E]),
            m_vecSupportedSessions: RVec::from(vec![0x01, 0x02, 0x03]),
            m_vecDids: RVec::from(vec![RDataIdentifier {
                m_u16Id: 0xF190,
                m_vecValue: RVec::from(b"VIN0123456789XYZ".to_vec()),
            }]),
            m_vecDtcs: RVec::from(vec![RDtc {
                m_u32Code: 0x123456,
                m_byStatus: 0x2F,
            }]),
            m_byExpectedBlockSequenceCounter: 0,
            m_bIsTransferInProgress: false,
            m_bIsPermissive: false,
            m_vecSecurityLevels: RVec::from(vec![RSecurityLevel {
                m_byRequestSeedSubFunction: 0x01,
                m_vecSeed: RVec::from(vec![0x11, 0x22, 0x33, 0x44]),
                m_vecExpectedKey: RVec::from(vec![0xAA, 0xBB, 0xCC, 0xDD]),
                m_keyPolicy: RKeyPolicy::CompareWithExpectedKey,
                m_byRefusalNrc: 0,
            }]),
        }
    }

    #[test]
    fn unsupported_service_is_rejected() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        // 0x28 (CommunicationControl) is not in the supported list.
        let reply = HandleRequest(&[0x28, 0x00], &snapshot);
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x28, c_byNrcServiceNotSupported]
        );
    }

    #[test]
    fn session_control_switches_session() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x10, 0x03], &snapshot);
        assert_eq!(reply.m_vecResponse[0], 0x50);
        assert_eq!(reply.m_vecResponse[1], 0x03);
        assert_eq!(reply.m_vecChanges.len(), 1);
        assert_eq!(reply.m_vecChanges[0].m_byKind, c_byStateChangeSetSession);
        assert_eq!(reply.m_vecChanges[0].m_byValue, 0x03);
    }

    #[test]
    fn session_control_rejects_unsupported_session() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        // 0x04 (SafetySystem) is not supported by this ECU.
        let reply = HandleRequest(&[0x10, 0x04], &snapshot);
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x10, c_byNrcSubFunctionNotSupported]
        );
    }

    #[test]
    fn session_control_suppresses_positive_response_but_still_switches() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x10, 0x83], &snapshot); // 0x83 = 0x03 | suppress bit
        assert!(reply.m_vecResponse.is_empty());
        assert_eq!(reply.m_vecChanges[0].m_byValue, 0x03);
    }

    #[test]
    fn read_did_returns_value() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x22, 0xF1, 0x90], &snapshot);
        assert_eq!(&reply.m_vecResponse[0..3], &[0x62, 0xF1, 0x90]);
        assert_eq!(&reply.m_vecResponse[3..], b"VIN0123456789XYZ");
    }

    #[test]
    fn read_unknown_did_is_out_of_range() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x22, 0x12, 0x34], &snapshot);
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x22, c_byNrcRequestOutOfRange]
        );
    }

    #[test]
    fn security_access_denied_in_default_session() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x27, 0x01], &snapshot);
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x27, c_byNrcServiceNotSupportedInActiveSession]
        );
    }

    #[test]
    fn security_request_seed_returns_seed_and_marks_active() {
        let snapshot = MakeSnapshot(0x03, 0x00, 0x00); // extended session
        let reply = HandleRequest(&[0x27, 0x01], &snapshot);
        assert_eq!(&reply.m_vecResponse[0..2], &[0x67, 0x01]);
        assert_eq!(&reply.m_vecResponse[2..], &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(
            reply.m_vecChanges[0].m_byKind,
            c_byStateChangeSetActiveSeedLevel
        );
        assert_eq!(reply.m_vecChanges[0].m_byValue, 0x01);
    }

    #[test]
    fn security_send_correct_key_unlocks() {
        // Seed already requested (active seed level = 0x01).
        let snapshot = MakeSnapshot(0x03, 0x00, 0x01);
        let reply = HandleRequest(&[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x67, 0x02]);
        assert_eq!(
            reply.m_vecChanges[0].m_byKind,
            c_byStateChangeUnlockSecurity
        );
        assert_eq!(reply.m_vecChanges[0].m_byValue, 0x01);
    }

    #[test]
    fn security_send_wrong_key_is_invalid_key() {
        let snapshot = MakeSnapshot(0x03, 0x00, 0x01);
        let reply = HandleRequest(&[0x27, 0x02, 0x00, 0x00, 0x00, 0x00], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x7F, 0x27, c_byNrcInvalidKey]);
    }

    #[test]
    fn security_send_key_without_seed_is_sequence_error() {
        let snapshot = MakeSnapshot(0x03, 0x00, 0x00); // no active seed
        let reply = HandleRequest(&[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD], &snapshot);
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x27, c_byNrcRequestSequenceError]
        );
    }

    #[test]
    fn read_dtc_by_status_mask_lists_matching_dtcs() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x19, 0x02, 0x08], &snapshot);
        assert_eq!(&reply.m_vecResponse[0..3], &[0x59, 0x02, 0xFF]);
        assert_eq!(&reply.m_vecResponse[3..], &[0x12, 0x34, 0x56, 0x2F]);
    }

    #[test]
    fn tester_present_is_acknowledged() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x3E, 0x00], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x7E, 0x00]);
    }

    #[test]
    fn routine_control_echoes_routine_id() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x31, 0x01, 0x02, 0x03], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x71, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn wrong_length_is_rejected() {
        let snapshot = MakeSnapshot(0x01, 0x00, 0x00);
        let reply = HandleRequest(&[0x22, 0xF1], &snapshot); // DID needs 2 bytes
        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x22, c_byNrcIncorrectMessageLength]
        );
    }

    /// Build a snapshot whose single security level uses the given policy, with the seed
    /// already requested so the sequence rule is satisfied.
    fn SnapshotWithPolicy(keyPolicy: RKeyPolicy, byRefusalNrc: u8) -> REcuSnapshot {
        // Extended session, nothing unlocked, seed 0x01 already handed out.
        let mut snapshot = MakeSnapshot(0x03, 0x00, 0x01);
        snapshot.m_vecSecurityLevels = RVec::from(vec![RSecurityLevel {
            m_byRequestSeedSubFunction: 0x01,
            m_vecSeed: RVec::from(vec![0x11, 0x22, 0x33, 0x44]),
            m_vecExpectedKey: RVec::from(vec![0xAA, 0xBB, 0xCC, 0xDD]),
            m_keyPolicy: keyPolicy,
            m_byRefusalNrc: byRefusalNrc,
        }]);
        snapshot
    }

    #[test]
    fn a_level_that_accepts_any_key_unlocks_on_a_key_it_has_never_seen() {
        // The reconstruction case: the seed came from a capture, the key did not, and
        // comparing would refuse every tester with 0x35.
        let snapshot = SnapshotWithPolicy(RKeyPolicy::AcceptAnyKey, 0);
        let reply = HandleRequest(&[0x27, 0x02, 0xDE, 0xAD, 0xBE, 0xEF], &snapshot);

        assert_eq!(&reply.m_vecResponse[..2], &[0x67, 0x02]);
        assert!(
            reply
                .m_vecChanges
                .iter()
                .any(|change| change.m_byKind == c_byStateChangeUnlockSecurity),
            "accepting the key must actually unlock, not merely answer positively"
        );
    }

    #[test]
    fn a_refusing_level_answers_with_the_code_it_was_given() {
        // Fault injection: the tester's rejected-key path, on demand. 0x36 is
        // exceededNumberOfAttempts, which a tester handles differently from 0x35.
        let snapshot = SnapshotWithPolicy(RKeyPolicy::RefuseWith, 0x36);
        let reply = HandleRequest(&[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD], &snapshot);

        assert_eq!(
            reply.m_vecResponse,
            vec![0x7F, 0x27, 0x36],
            "even the correct key is refused, with the configured code"
        );
        assert!(reply.m_vecChanges.is_empty(), "a refusal unlocks nothing");
    }

    #[test]
    fn accepting_any_key_still_refuses_one_that_arrives_without_a_seed() {
        // ISO 14229-1 makes seed-before-key a sequence rule, not a key rule, so relaxing the
        // key comparison must not relax the ordering too.
        let mut snapshot = SnapshotWithPolicy(RKeyPolicy::AcceptAnyKey, 0);
        snapshot.m_byActiveSeedLevel = 0x00;

        let reply = HandleRequest(&[0x27, 0x02, 0xDE, 0xAD], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x7F, 0x27, 0x24]);
    }

    #[test]
    fn comparing_is_still_what_a_level_with_a_known_key_does() {
        let snapshot = SnapshotWithPolicy(RKeyPolicy::CompareWithExpectedKey, 0);

        let wrong = HandleRequest(&[0x27, 0x02, 0x00, 0x00, 0x00, 0x00], &snapshot);
        assert_eq!(wrong.m_vecResponse, vec![0x7F, 0x27, 0x35]);

        let right = HandleRequest(&[0x27, 0x02, 0xAA, 0xBB, 0xCC, 0xDD], &snapshot);
        assert_eq!(&right.m_vecResponse[..2], &[0x67, 0x02]);
    }

    /// Run a download from RequestDownload through to RequestTransferExit, carrying the block
    /// counter forward the way the ECU does.
    fn RunTransfer(byStartCounter: u8, arrBlocks: &[u8]) -> Vec<Vec<u8>> {
        let mut snapshot = Snapshot();
        snapshot.m_byExpectedBlockSequenceCounter = byStartCounter;
        snapshot.m_bIsTransferInProgress = true;

        let mut vecResponses = Vec::new();
        for byBlock in arrBlocks {
            let reply = HandleRequest(&[0x36, *byBlock, 0xAA], &snapshot);
            for change in &reply.m_vecChanges {
                if change.m_byKind == c_byStateChangeSetBlockSequenceCounter {
                    snapshot.m_byExpectedBlockSequenceCounter = change.m_byValue;
                }
            }
            vecResponses.push(reply.m_vecResponse);
        }
        vecResponses
    }

    #[test]
    fn a_transfer_starts_at_one_and_echoes_each_block_counter() {
        let snapshot = Snapshot();
        let reply = HandleRequest(&[0x34, 0x00, 0x44, 0, 0, 0, 0, 0, 0, 0x01, 0x00], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x74, 0x20, 0x04, 0x00]);
        assert_eq!(
            reply.m_vecChanges[0].m_byValue, 0x01,
            "the first TransferData of a transfer carries 0x01"
        );

        let vecResponses = RunTransfer(0x01, &[0x01, 0x02, 0x03]);
        assert_eq!(
            vecResponses,
            vec![vec![0x76, 0x01], vec![0x76, 0x02], vec![0x76, 0x03]]
        );
    }

    #[test]
    fn the_block_counter_wraps_from_ff_to_zero_not_back_to_one() {
        // The value implementations most often get wrong: 0x01 is only the *first* block of a
        // transfer, so the wrap goes to 0x00 and the sequence continues from there.
        let vecResponses = RunTransfer(0xFE, &[0xFE, 0xFF, 0x00, 0x01]);
        assert_eq!(
            vecResponses,
            vec![
                vec![0x76, 0xFE],
                vec![0x76, 0xFF],
                vec![0x76, 0x00],
                vec![0x76, 0x01]
            ]
        );
    }

    #[test]
    fn a_repeated_block_is_answered_again_rather_than_refused() {
        // ISO 14229-1 clause 14.2.5: the tester is saying its copy of the response went
        // missing, not that it has lost its place.
        let mut snapshot = Snapshot();
        snapshot.m_byExpectedBlockSequenceCounter = 0x05;
        snapshot.m_bIsTransferInProgress = true;

        let reply = HandleRequest(&[0x36, 0x04, 0xAA], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x76, 0x04]);
        assert!(
            reply.m_vecChanges.is_empty(),
            "a retransmission must not advance the counter"
        );
    }

    #[test]
    fn a_block_out_of_sequence_is_refused_with_seventy_three() {
        let mut snapshot = Snapshot();
        snapshot.m_byExpectedBlockSequenceCounter = 0x05;
        snapshot.m_bIsTransferInProgress = true;

        let reply = HandleRequest(&[0x36, 0x09, 0xAA], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x7F, 0x36, 0x73]);
    }

    #[test]
    fn transfer_data_before_any_request_download_is_a_sequence_error() {
        let snapshot = Snapshot();
        let reply = HandleRequest(&[0x36, 0x01, 0xAA], &snapshot);
        assert_eq!(reply.m_vecResponse, vec![0x7F, 0x36, 0x24]);
    }

    #[test]
    fn permissive_mode_stops_the_supported_service_list_refusing() {
        // 0x2F is not in the fixture's list. Strict, it is refused; permissive, it reaches the
        // dispatch — which still has no handler for it, because permissive never invents.
        let mut snapshot = Snapshot();
        assert_eq!(
            HandleRequest(&[0x2F, 0x01, 0x02, 0x03], &snapshot).m_vecResponse,
            vec![0x7F, 0x2F, 0x11]
        );

        snapshot.m_bIsPermissive = true;
        assert_eq!(
            HandleRequest(&[0x2F, 0x01, 0x02, 0x03], &snapshot).m_vecResponse,
            vec![0x7F, 0x2F, 0x11],
            "permissive lets the request through the gate; it does not answer for it"
        );

        // A service that does have a handler is reached, though, which is the point.
        snapshot.m_vecSupportedServices = RVec::from(vec![0x10]);
        assert_eq!(
            HandleRequest(&[0x36, 0x01, 0xAA], &snapshot).m_vecResponse,
            vec![0x7F, 0x36, 0x24],
            "0x36 is absent from the list but permissive reaches its handler"
        );
    }

    /// An ECU in the extended session that declares the transfer services, as one being
    /// flashed would. Without them the supported-service gate refuses before any handler runs
    /// — which is itself worth knowing: implementing 0x36 is not enough on its own.
    fn Snapshot() -> REcuSnapshot {
        let mut snapshot = MakeSnapshot(0x03, 0x00, 0x00);
        snapshot.m_vecSupportedServices = RVec::from(vec![
            0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x34, 0x35, 0x36, 0x37, 0x3E,
        ]);
        snapshot
    }
}
