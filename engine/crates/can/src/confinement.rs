//! Fault confinement: how a CAN node reacts to errors, and when it stops talking.
//!
//! ISO 11898-1 clause 12.1. Every node keeps two error counters and is in one of three states
//! derived from them. The states are not cosmetic — they change what the node is allowed to put
//! on the wire, and the last of them takes it off the bus entirely. A tester that has never
//! been made to face a bus-off ECU has never been tested against one.
//!
//! This is a model of the state machine, not a CAN controller. Real counters move on bit
//! errors, stuff errors, CRC errors and form errors detected in silicon; nothing here detects
//! those, because a simulator has no wire to detect them on. What it offers instead is the
//! transitions an operator wants to provoke, reached by injecting the errors that cause them,
//! so the states and the rules between them are real even though their cause is deliberate.

#![allow(non_snake_case, non_upper_case_globals)]

use serde::{Deserialize, Serialize};

/// Error count above which a node becomes error-passive (ISO 11898-1 clause 12.1.4.2).
pub const c_u16ErrorPassiveThreshold: u16 = 127;
/// Transmit error count above which a node goes bus-off.
pub const c_u16BusOffThreshold: u16 = 255;
/// What a transmit error adds to the transmit counter (clause 12.1.4.2 rule 2).
pub const c_u16TransmitErrorPenalty: u16 = 8;
/// What a receive error adds to the receive counter (rule 1).
pub const c_u16ReceiveErrorPenalty: u16 = 1;
/// What a successful transmission subtracts (rule 4).
pub const c_u16TransmitSuccessCredit: u16 = 1;

/// How much of the bus a node is currently allowed to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BusState {
    /// Normal. Transmits, receives, and signals errors actively.
    #[default]
    ErrorActive,
    /// Still on the bus and still answering, but signalling errors passively and waiting an
    /// extra eight bits before starting a frame — so it loses arbitration it would otherwise
    /// win. A tester sees an ECU that answers more slowly and sometimes not at all.
    ErrorPassive,
    /// Off the bus. Transmits nothing and acknowledges nothing, so every request to it times
    /// out rather than being refused. The distinction matters to a tester: a refusal is an
    /// answer, and silence is not.
    BusOff,
}

impl BusState {
    /// Whether a node in this state may put a frame on the bus at all.
    pub fn CanTransmit(self) -> bool {
        !matches!(self, BusState::BusOff)
    }

    /// A phrase for a log line or the UI.
    pub fn Describe(self) -> &'static str {
        match self {
            BusState::ErrorActive => "error active",
            BusState::ErrorPassive => "error passive",
            BusState::BusOff => "bus off",
        }
    }
}

/// A node's error counters and the state they put it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FaultConfinement {
    /// Transmit error counter. Only this one can reach bus-off.
    pub m_u16TransmitErrorCount: u16,
    /// Receive error counter. Can make a node error-passive but never bus-off, because a node
    /// that only listens badly is not the one disrupting the bus.
    pub m_u16ReceiveErrorCount: u16,
    /// True once the counters have driven the node off the bus, and until it is recovered.
    ///
    /// Held separately from the counters because leaving bus-off is not a matter of counting
    /// back down: ISO 11898-1 requires 128 occurrences of 11 consecutive recessive bits before
    /// a node may return, which is a decision to rejoin rather than a counter decrementing.
    pub m_bIsBusOff: bool,
}

impl FaultConfinement {
    /// The state these counters put the node in.
    pub fn State(&self) -> BusState {
        if self.m_bIsBusOff {
            return BusState::BusOff;
        }
        let bIsPassive = self.m_u16TransmitErrorCount > c_u16ErrorPassiveThreshold
            || self.m_u16ReceiveErrorCount > c_u16ErrorPassiveThreshold;
        if bIsPassive {
            BusState::ErrorPassive
        } else {
            BusState::ErrorActive
        }
    }

    /// Record a transmit error, which is the one that can end in bus-off.
    pub fn OnTransmitError(&mut self) {
        if self.m_bIsBusOff {
            return;
        }
        self.m_u16TransmitErrorCount = self
            .m_u16TransmitErrorCount
            .saturating_add(c_u16TransmitErrorPenalty);

        if self.m_u16TransmitErrorCount > c_u16BusOffThreshold {
            self.m_bIsBusOff = true;
        }
    }

    /// Record a receive error. It can make a node error-passive and no further: a node that
    /// mis-hears is not the one corrupting the bus, so it is never removed from it.
    pub fn OnReceiveError(&mut self) {
        if self.m_bIsBusOff {
            return;
        }
        self.m_u16ReceiveErrorCount = self
            .m_u16ReceiveErrorCount
            .saturating_add(c_u16ReceiveErrorPenalty)
            .min(c_u16BusOffThreshold);
    }

    /// Record a frame sent without error, which earns back one count.
    pub fn OnTransmitSuccess(&mut self) {
        if self.m_bIsBusOff {
            return;
        }
        self.m_u16TransmitErrorCount = self
            .m_u16TransmitErrorCount
            .saturating_sub(c_u16TransmitSuccessCredit);
    }

    /// Record a frame received without error.
    pub fn OnReceiveSuccess(&mut self) {
        if self.m_bIsBusOff {
            return;
        }
        self.m_u16ReceiveErrorCount = self.m_u16ReceiveErrorCount.saturating_sub(1);
    }

    /// Rejoin the bus with both counters cleared, as a node does after bus-off recovery.
    pub fn Recover(&mut self) {
        self.m_u16TransmitErrorCount = 0;
        self.m_u16ReceiveErrorCount = 0;
        self.m_bIsBusOff = false;
    }

    /// Put the node straight into a state, for an operator who wants the condition rather than
    /// the sequence of errors that reaches it.
    ///
    /// The counters are set to a value consistent with the state, so a node forced
    /// error-passive and then given one more transmit error behaves as one that arrived there
    /// by counting would — the shortcut does not leave the model in a state it could not have
    /// reached on its own.
    pub fn ForceState(&mut self, state: BusState) {
        match state {
            BusState::ErrorActive => self.Recover(),
            BusState::ErrorPassive => {
                self.m_bIsBusOff = false;
                self.m_u16TransmitErrorCount = c_u16ErrorPassiveThreshold + 1;
                self.m_u16ReceiveErrorCount = 0;
            }
            BusState::BusOff => {
                self.m_bIsBusOff = true;
                self.m_u16TransmitErrorCount = c_u16BusOffThreshold + 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_node_is_error_active() {
        let confinement = FaultConfinement::default();
        assert_eq!(confinement.State(), BusState::ErrorActive);
        assert!(confinement.State().CanTransmit());
    }

    #[test]
    fn transmit_errors_walk_a_node_through_every_state() {
        // Eight per error, so passive at 128 and off past 255 — sixteen errors to passive,
        // thirty-two to off. Counting them out is the point: the thresholds are the standard's,
        // not a round number chosen here.
        let mut confinement = FaultConfinement::default();
        for _ in 0..16 {
            confinement.OnTransmitError();
        }
        assert_eq!(confinement.m_u16TransmitErrorCount, 128);
        assert_eq!(confinement.State(), BusState::ErrorPassive);
        assert!(
            confinement.State().CanTransmit(),
            "error-passive still answers; that is what separates it from bus-off"
        );

        for _ in 0..16 {
            confinement.OnTransmitError();
        }
        assert_eq!(confinement.State(), BusState::BusOff);
        assert!(!confinement.State().CanTransmit());
    }

    #[test]
    fn receive_errors_can_reach_passive_but_never_bus_off() {
        // A node that mis-hears is not the one corrupting the bus, so the standard never
        // removes it for that alone.
        let mut confinement = FaultConfinement::default();
        for _ in 0..1000 {
            confinement.OnReceiveError();
        }
        assert_eq!(confinement.State(), BusState::ErrorPassive);
        assert!(confinement.State().CanTransmit());
        assert!(!confinement.m_bIsBusOff);
    }

    #[test]
    fn a_bus_off_node_stays_off_until_it_is_recovered() {
        // Counting back down is not how a node returns: ISO 11898-1 requires 128 occurrences of
        // eleven recessive bits, which is a decision to rejoin, not a decrement.
        let mut confinement = FaultConfinement::default();
        confinement.ForceState(BusState::BusOff);

        for _ in 0..500 {
            confinement.OnTransmitSuccess();
            confinement.OnReceiveSuccess();
        }
        assert_eq!(
            confinement.State(),
            BusState::BusOff,
            "success does not undo it"
        );

        confinement.Recover();
        assert_eq!(confinement.State(), BusState::ErrorActive);
        assert_eq!(confinement.m_u16TransmitErrorCount, 0);
    }

    #[test]
    fn forcing_a_state_leaves_counters_that_state_could_have_reached() {
        // The shortcut must not produce a node that is passive with a count of zero — one more
        // error would then behave as though the first sixteen had not happened.
        let mut confinement = FaultConfinement::default();
        confinement.ForceState(BusState::ErrorPassive);
        assert!(confinement.m_u16TransmitErrorCount > c_u16ErrorPassiveThreshold);

        for _ in 0..16 {
            confinement.OnTransmitError();
        }
        assert_eq!(
            confinement.State(),
            BusState::BusOff,
            "a forced-passive node reaches bus-off in the same number of errors as a counted one"
        );
    }

    #[test]
    fn success_earns_back_one_count_at_a_time() {
        let mut confinement = FaultConfinement::default();
        confinement.OnTransmitError();
        assert_eq!(confinement.m_u16TransmitErrorCount, 8);

        for _ in 0..8 {
            confinement.OnTransmitSuccess();
        }
        assert_eq!(confinement.m_u16TransmitErrorCount, 0);

        // And never below zero.
        confinement.OnTransmitSuccess();
        assert_eq!(confinement.m_u16TransmitErrorCount, 0);
    }
}
