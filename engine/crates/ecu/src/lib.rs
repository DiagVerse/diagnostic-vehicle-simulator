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

use abi_stable::std_types::RVec;
use application::ProtocolHandler;
use core_domain::model::Ecu;
use plugin_contract::protocol::{
    c_byStateChangeResetToDefaultSession, c_byStateChangeSetActiveSeedLevel,
    c_byStateChangeSetSession, c_byStateChangeUnlockSecurity, RDataIdentifier, RDtc, REcuSnapshot,
    RSecurityLevel, RStateChange,
};

/// UDS default-session sub-function; the session an ECU powers up in.
const c_bySessionDefault: u8 = 0x01;

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

    /// Process one diagnostic request: snapshot state, ask the protocol for a response and
    /// state changes, apply them, and return the response bytes (empty = suppressed).
    pub fn ProcessRequest(&mut self, protocol: &dyn ProtocolHandler, vecRequest: &[u8]) -> Vec<u8> {
        let snapshot = self.BuildSnapshot();

        let outcome = protocol.Handle(RVec::from(vecRequest.to_vec()), snapshot);

        for change in outcome.m_vecChanges.iter() {
            self.ApplyStateChange(change);
        }

        outcome.m_vecResponse.into_vec()
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
