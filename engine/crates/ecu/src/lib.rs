//! Virtual ECU runtime.
//!
//! A [`VirtualEcu`] owns the **live, mutable** diagnostic state of one running ECU (the
//! current session, whether security is unlocked, any outstanding seed) and drives a
//! protocol handler to answer requests. It is deliberately state-aware, not a packet
//! replayer (README §13): for each request it builds a snapshot of its state, asks the
//! protocol plugin for a response plus the state changes to apply, applies them, and returns
//! the response bytes.
//!
//! The ECU depends only on the `ProtocolHandler` trait, so it neither knows nor cares whether
//! the protocol is a dynamically-loaded plugin or an in-process implementation.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod sample;
pub mod schedule;

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::{c_u32P2StarResolutionMs, Ecu, EcuTiming};
use plugin_contract::protocol::{
    c_byStateChangeResetToDefaultSession, c_byStateChangeSetActiveSeedLevel,
    c_byStateChangeSetSession, c_byStateChangeUnlockSecurity, RDataIdentifier, RDtc, REcuSnapshot,
    RSecurityLevel, RStateChange,
};

use crate::schedule::{BuildResponsePlan, ResolveResponsePendingCount, ResponsePlan};

/// UDS default-session sub-function; the session an ECU powers up in.
const c_bySessionDefault: u8 = 0x01;

/// Positive response SID for DiagnosticSessionControl (0x10 + 0x40).
const c_bySessionControlPositiveResponse: u8 = 0x50;
/// Length of a conformant DiagnosticSessionControl positive response: SID, echoed
/// sub-function, and the four-byte sessionParameterRecord (ISO 14229-1 Table 28).
const c_uSessionControlResponseLength: usize = 6;
/// UDS services whose sub-function byte carries the suppressPosRspMsgIndicationBit
/// (ISO 14229-1 Table 11): DiagnosticSessionControl, ECUReset, CommunicationControl,
/// RoutineControl, TesterPresent, AccessTimingParameter, ControlDTCSetting, ResponseOnEvent
/// and LinkControl.
///
/// This has to be a whitelist rather than "byte 1 of any request": in a
/// ReadDataByIdentifier, byte 1 is the high byte of the DID, so clearing bit 7 would silently
/// change which DID is read (`22 F1 90` would become `22 71 90`) instead of un-suppressing
/// anything. Services with a sub-function that never suppress — SecurityAccess,
/// ReadDTCInformation — are excluded for the same reason.
const c_arrSuppressCapableServices: [u8; 9] =
    [0x10, 0x11, 0x28, 0x31, 0x3E, 0x83, 0x85, 0x86, 0x87];

/// Bit 7 of a sub-function byte: suppressPosRspMsgIndicationBit (ISO 14229-1 Table 11).
const c_bySuppressPositiveResponseBit: u8 = 0x80;

/// A running virtual ECU: static configuration plus live diagnostic state.
pub struct VirtualEcu {
    /// Static configuration (services, DIDs, DTCs, security levels) from the vehicle model.
    m_config: Ecu,
    /// Current session as its UDS sub-function byte.
    m_byCurrentSession: u8,
    /// Currently unlocked security level (0 = locked).
    m_bySecurityUnlockedLevel: u8,
    /// Security level for which a seed was most recently issued (0 = none outstanding).
    m_byActiveSeedLevel: u8,
}

impl VirtualEcu {
    /// Create a running ECU from its configuration. It starts in the default session with
    /// security locked.
    pub fn New(config: Ecu) -> Self {
        VirtualEcu {
            m_config: config,
            m_byCurrentSession: c_bySessionDefault,
            m_bySecurityUnlockedLevel: 0,
            m_byActiveSeedLevel: 0,
        }
    }

    /// The ECU's static configuration.
    pub fn Config(&self) -> &Ecu {
        &self.m_config
    }

    /// The current session sub-function byte.
    pub fn CurrentSession(&self) -> u8 {
        self.m_byCurrentSession
    }

    /// The currently unlocked security level (0 = locked).
    pub fn SecurityUnlockedLevel(&self) -> u8 {
        self.m_bySecurityUnlockedLevel
    }

    /// Whether any security level is unlocked.
    pub fn IsSecurityUnlocked(&self) -> bool {
        self.m_bySecurityUnlockedLevel != 0
    }

    /// Rename the ECU. Configuration only — no diagnostic state changes.
    pub fn SetName(&mut self, strName: &str) {
        tracing::info!(from = %self.m_config.m_strName, to = %strName, "ECU renamed");
        self.m_config.m_strName = strName.to_string();
    }

    /// The ECU's timing parameters.
    pub fn Timing(&self) -> EcuTiming {
        self.m_config.m_timing
    }

    /// Replace the ECU's timing parameters.
    ///
    /// The caller is responsible for validating them first ([`EcuTiming::Validate`]). A change
    /// affects the **next** request only: a response plan already computed keeps the values it
    /// was built with, and the tester learns the new P2/P2* at the next
    /// DiagnosticSessionControl response, which is the only place ISO 14229-1 carries them.
    /// Timing is configuration, so it survives a diagnostic reset.
    pub fn SetTiming(&mut self, timing: EcuTiming) {
        tracing::info!(
            ecu = %self.m_config.m_strName,
            p2Ms = timing.m_u32P2ServerMaxMs,
            p2StarMs = timing.m_u32P2StarServerMaxMs,
            delayMs = timing.m_u32ResponseDelayMs,
            forcePending = timing.m_bForceResponsePending,
            "ECU timing updated"
        );
        self.m_config.m_timing = timing;
    }

    /// Process one diagnostic request and return only the final response bytes (empty =
    /// nothing is sent).
    ///
    /// This is the instantaneous path: it discards the schedule, so no delay is applied and
    /// any ResponsePending messages are dropped. It backs the dev `/ecu/*` endpoints and the
    /// in-process tests, where timing is not the subject. The `/simulation/*` path uses
    /// [`VirtualEcu::ProcessRequestWithTiming`] and honours the schedule in full.
    pub fn ProcessRequest(&mut self, protocol: &dyn ProtocolHandler, vecRequest: &[u8]) -> Vec<u8> {
        let plan = self.ProcessRequestWithTiming(protocol, vecRequest);
        plan.FinalResponse().to_vec()
    }

    /// Process one diagnostic request and return the whole timed answer: the ResponsePending
    /// messages, the final response, and when each goes on the wire.
    ///
    /// State changes are applied here, once, exactly as before — the plan describes only what
    /// is transmitted. The caller executes the plan against a real clock; nothing in this
    /// crate sleeps.
    pub fn ProcessRequestWithTiming(
        &mut self,
        protocol: &dyn ProtocolHandler,
        vecRequest: &[u8],
    ) -> ResponsePlan {
        if vecRequest.is_empty() {
            return BuildResponsePlan(&self.m_config.m_timing, 0x00, &[], 0);
        }

        let byRequestSid = vecRequest[0];
        let u8PendingCount = self.ResolvePendingCountFor(byRequestSid);

        // A ResponsePending sequence obliges the server to send a final response whatever the
        // suppressPosRspMsgIndicationBit says (ISO 14229-1 Annex A.1, and the third condition
        // of the clause 7.5.5 pseudocode). The handler is a pure function that only reports
        // what it was asked for, so the bit is cleared before asking rather than trying to
        // recover a response it was never told to produce.
        let vecEffectiveRequest = self.ClearSuppressBitIfPending(vecRequest, u8PendingCount);

        let snapshot = self.BuildSnapshot();
        let outcome = protocol.Handle(RVec::from(vecEffectiveRequest), snapshot);

        for change in outcome.m_vecChanges.iter() {
            self.ApplyStateChange(change);
        }

        let mut vecResponse = outcome.m_vecResponse.into_vec();
        self.ApplySessionTimingRecord(&mut vecResponse);

        BuildResponsePlan(
            &self.m_config.m_timing,
            byRequestSid,
            &vecResponse,
            u8PendingCount,
        )
    }

    /// How many ResponsePending messages this request may carry.
    ///
    /// Two cases forbid NRC 0x78 outright, both from ISO 14229-2 clause 7.1.1: an unsupported
    /// service (whose P4Server_max always equals P2Server_max), and any server configured with
    /// `P4 == P2`. Both are decidable before the handler runs, which is what lets the
    /// suppress-bit decision above be made in time.
    fn ResolvePendingCountFor(&self, byRequestSid: u8) -> u8 {
        let bIsServiceSupported = self.m_config.IsServiceSupported(byRequestSid);
        let bHasEnhancedBudget =
            self.m_config.m_timing.m_u32P4ServerMaxMs > self.m_config.m_timing.m_u32P2ServerMaxMs;

        if !bIsServiceSupported || !bHasEnhancedBudget {
            return 0;
        }

        ResolveResponsePendingCount(&self.m_config.m_timing)
    }

    /// Clear the suppressPosRspMsgIndicationBit when a ResponsePending sequence is planned, so
    /// the handler produces the final response the standard requires.
    fn ClearSuppressBitIfPending(&self, vecRequest: &[u8], u8PendingCount: u8) -> Vec<u8> {
        if u8PendingCount == 0 || vecRequest.len() < 2 {
            return vecRequest.to_vec();
        }

        let bHasSubFunction = c_arrSuppressCapableServices.contains(&vecRequest[0]);
        let bIsSuppressRequested = (vecRequest[1] & c_bySuppressPositiveResponseBit) != 0;

        if !bHasSubFunction || !bIsSuppressRequested {
            return vecRequest.to_vec();
        }

        tracing::info!(
            ecu = %self.m_config.m_strName,
            sid = format!("{:02X}", vecRequest[0]),
            "suppressPosRspMsgIndicationBit overridden: a ResponsePending sequence obliges the server to send a final response (ISO 14229-1 Annex A.1)"
        );

        let mut vecEffective = vecRequest.to_vec();
        vecEffective[1] &= !c_bySuppressPositiveResponseBit;
        vecEffective
    }

    /// Stamp this ECU's live P2/P2* into a DiagnosticSessionControl positive response.
    ///
    /// The sessionParameterRecord is a session-layer parameter (ISO 14229-2), not application
    /// semantics, so it is applied here rather than inside the transport-agnostic UDS plugin —
    /// the same split ADR 0004 made for functional NRC suppression. Layout and scaling come
    /// from ISO 14229-1 clause 9.2.3.1, Tables 28 and 29: P2Server_max in two bytes at 1 ms
    /// resolution, P2*Server_max in two bytes at 10 ms resolution.
    fn ApplySessionTimingRecord(&self, vecResponse: &mut [u8]) {
        if vecResponse.first() != Some(&c_bySessionControlPositiveResponse) {
            return;
        }

        let timing = &self.m_config.m_timing;
        let u32P2StarUnits = timing.m_u32P2StarServerMaxMs / c_u32P2StarResolutionMs;
        let arrRecord = [
            (timing.m_u32P2ServerMaxMs >> 8) as u8,
            timing.m_u32P2ServerMaxMs as u8,
            (u32P2StarUnits >> 8) as u8,
            u32P2StarUnits as u8,
        ];

        if vecResponse.len() >= c_uSessionControlResponseLength {
            vecResponse[2..c_uSessionControlResponseLength].copy_from_slice(&arrRecord);
            return;
        }

        // Anything shorter has no record to stamp. ISO 14229-1 Table 28 marks all four bytes
        // mandatory, so such a response is not conformant — but the bytes belong to whichever
        // protocol plugin produced them, and inventing four to cover for it would hide the
        // real problem. Report it and leave it alone.
        tracing::warn!(
            ecu = %self.m_config.m_strName,
            len = vecResponse.len(),
            "DiagnosticSessionControl positive response is shorter than the six bytes ISO 14229-1 Table 28 requires; its timing record was left untouched"
        );
    }

    /// Build the FFI-safe snapshot the protocol handler needs, from config + live state.
    fn BuildSnapshot(&self) -> REcuSnapshot {
        let vecDids: Vec<RDataIdentifier> = self
            .m_config
            .m_mapDids
            .values()
            .map(|did| RDataIdentifier {
                m_u16Id: did.m_u16Id,
                m_vecValue: RVec::from(did.m_vecValue.clone()),
            })
            .collect();

        let vecDtcs: Vec<RDtc> = self
            .m_config
            .m_vecDtcs
            .iter()
            .map(|dtc| RDtc {
                m_u32Code: dtc.m_u32Code,
                m_byStatus: dtc.m_byStatus,
            })
            .collect();

        let vecSecurityLevels: Vec<RSecurityLevel> = self
            .m_config
            .m_vecSecurityLevels
            .iter()
            .map(|level| RSecurityLevel {
                m_byRequestSeedSubFunction: level.m_byRequestSeedSubFunction,
                m_vecSeed: RVec::from(level.m_vecSeed.clone()),
                m_vecExpectedKey: RVec::from(level.m_vecExpectedKey.clone()),
            })
            .collect();

        let vecSupportedSessions: Vec<u8> = self
            .m_config
            .m_vecSupportedSessions
            .iter()
            .map(|session| session.ToSubFunction())
            .collect();

        REcuSnapshot {
            m_byCurrentSession: self.m_byCurrentSession,
            m_bySecurityUnlockedLevel: self.m_bySecurityUnlockedLevel,
            m_byActiveSeedLevel: self.m_byActiveSeedLevel,
            m_vecSupportedServices: RVec::from(self.m_config.m_vecSupportedServices.clone()),
            m_vecSupportedSessions: RVec::from(vecSupportedSessions),
            m_vecDids: RVec::from(vecDids),
            m_vecDtcs: RVec::from(vecDtcs),
            m_vecSecurityLevels: RVec::from(vecSecurityLevels),
        }
    }

    /// Apply one state change requested by the protocol handler. Important transitions are
    /// logged so an operator can follow the ECU's behaviour from the logs alone.
    fn ApplyStateChange(&mut self, change: &RStateChange) {
        match change.m_byKind {
            c_byStateChangeSetSession => {
                let byNewSession = change.m_byValue;
                tracing::info!(
                    ecu = %self.m_config.m_strName,
                    from = self.m_byCurrentSession,
                    to = byNewSession,
                    "session changed"
                );
                self.m_byCurrentSession = byNewSession;
            }
            c_byStateChangeResetToDefaultSession => {
                tracing::info!(ecu = %self.m_config.m_strName, "ECU reset: returning to default session");
                self.m_byCurrentSession = c_bySessionDefault;
            }
            c_byStateChangeSetActiveSeedLevel => {
                self.m_byActiveSeedLevel = change.m_byValue;
            }
            c_byStateChangeUnlockSecurity => {
                let byLevel = change.m_byValue;
                if byLevel == 0 {
                    if self.m_bySecurityUnlockedLevel != 0 {
                        tracing::info!(ecu = %self.m_config.m_strName, "security relocked");
                    }
                } else {
                    tracing::info!(ecu = %self.m_config.m_strName, level = byLevel, "security unlocked");
                }
                self.m_bySecurityUnlockedLevel = byLevel;
            }
            byUnknown => {
                // Forward-compatibility: a newer plugin asked for a change this engine does
                // not understand. Log and ignore rather than misbehave.
                tracing::warn!(
                    ecu = %self.m_config.m_strName,
                    kind = byUnknown,
                    "ignoring unknown state change from protocol plugin"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_contract::protocol::RProtocolOutcome;

    /// A fake protocol handler that always switches to session 0x03 — lets us test the ECU
    /// state machine in isolation, without any real protocol.
    struct FakeSessionSwitcher;

    impl ProtocolHandler for FakeSessionSwitcher {
        fn Handle(&self, _vecRequest: RVec<u8>, _snapshot: REcuSnapshot) -> RProtocolOutcome {
            RProtocolOutcome {
                m_vecResponse: RVec::from(vec![0x50, 0x03]),
                m_vecChanges: RVec::from(vec![RStateChange {
                    m_byKind: c_byStateChangeSetSession,
                    m_byValue: 0x03,
                }]),
            }
        }

        fn Name(&self) -> &str {
            "fake"
        }
    }

    #[test]
    fn process_request_applies_state_changes() {
        let mut ecu = VirtualEcu::New(Ecu::New("Test_ECU", 0x1001));
        assert_eq!(ecu.CurrentSession(), 0x01);

        let response = ecu.ProcessRequest(&FakeSessionSwitcher, &[0x10, 0x03]);

        assert_eq!(response, vec![0x50, 0x03]);
        assert_eq!(ecu.CurrentSession(), 0x03);
    }
}
