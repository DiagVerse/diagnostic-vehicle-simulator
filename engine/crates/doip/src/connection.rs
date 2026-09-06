//! The TCP_DATA connection state machine (ISO 13400-2:2019 clause 12.6).
//!
//! Pure decision logic: a connection is given a decoded message and answers with what to send
//! and whether to close. No sockets, no clock — the timers are driven by the caller telling it
//! that time has passed. That is what makes the routing activation decision table, which is
//! where the standard is at its most intricate, testable without a network.

use crate::messages::{RoutingActivationOutcome, RoutingActivationRequest};

/// Where a connection has got to (clause 12.6.1.3).
///
/// Nothing diagnostic is answered *or routed* before `RoutingActive` (REQ 3.DoIP-131 NL) — a
/// diagnostic message on an `Initialized` socket is not even negatively acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// TCP established, nothing activated. The initial inactivity timer is running.
    Initialized,
    /// A routing activation request was accepted but authentication is outstanding.
    PendingAuthentication,
    /// Routing is active: diagnostic messages are routed.
    RoutingActive,
    /// Finished — the socket is to be closed and returned to listening.
    Finalize,
}

/// One live TCP_DATA connection.
///
/// Identified by its socket plus the source address activated on it: the standard keys a
/// logical connection on both (REQ 3.DoIP-152 NL), and the timers and authentication state are
/// per connection rather than per entity (REQ 3.DoIP-153 / 3.DoIP-154 NL).
#[derive(Debug, Clone)]
pub struct Connection {
    m_state: ConnectionState,
    /// The tester address registered on this socket, once one has been.
    m_optU16SourceAddress: Option<u16>,
    /// Milliseconds since anything was sent or received here.
    m_u64IdleMs: u64,
}

impl Default for Connection {
    fn default() -> Self {
        Connection::New()
    }
}

impl Connection {
    /// A freshly established socket.
    pub fn New() -> Self {
        Connection {
            m_state: ConnectionState::Initialized,
            m_optU16SourceAddress: None,
            m_u64IdleMs: 0,
        }
    }

    /// Where this connection has got to.
    pub fn State(&self) -> ConnectionState {
        self.m_state
    }

    /// The tester address registered here, if any.
    pub fn SourceAddress(&self) -> Option<u16> {
        self.m_optU16SourceAddress
    }

    /// True when diagnostic messages may be routed.
    pub fn IsRoutingActive(&self) -> bool {
        self.m_state == ConnectionState::RoutingActive
    }

    /// Note that something was sent or received.
    ///
    /// Resets the inactivity clock. Both directions count (REQ 3.DoIP-080 NL) — a response this
    /// entity sends keeps the socket alive just as a request does, and forgetting the outbound
    /// half is a listed trap.
    pub fn NoteActivity(&mut self) {
        self.m_u64IdleMs = 0;
    }

    /// Advance the clock, and say whether this connection has now timed out.
    ///
    /// Two different deadlines depending on the state: a socket that never activated gets the
    /// short initial timer, an activated one gets the long general timer. Both close the socket
    /// (REQ 3.DoIP-082 / 3.DoIP-086 NL).
    pub fn Tick(&mut self, u64ElapsedMs: u64) -> bool {
        if self.m_state == ConnectionState::Finalize {
            return true;
        }
        self.m_u64IdleMs = self.m_u64IdleMs.saturating_add(u64ElapsedMs);

        let u64DeadlineMs = match self.m_state {
            // Not yet activated: the short timer, a measure against connections that send
            // nothing or nothing valid.
            ConnectionState::Initialized => crate::timing::c_initialInactivity.as_millis() as u64,
            // Activated, or parked awaiting authentication — the long one. A socket legitimately
            // waiting for authentication must not be killed by the 2-second timer.
            _ => crate::timing::c_generalInactivity.as_millis() as u64,
        };

        if self.m_u64IdleMs >= u64DeadlineMs {
            self.m_state = ConnectionState::Finalize;
            return true;
        }
        false
    }

    /// Decide a routing activation request against this connection and the others.
    ///
    /// The decision table of clause 12.6.4, in the order Figure 26 applies it. Two of these
    /// are the listed traps: re-activating the *same* socket with the *same* source address is
    /// legal and must be accepted (REQ 3.DoIP-089 NL), and only a *different* address on an
    /// already-activated socket produces `0x02`.
    ///
    /// `bIsAddressAcceptable` is the entity's policy on which tester addresses it talks to —
    /// the standard never enumerates them, so that decision belongs to the caller.
    /// `bIsAddressActiveElsewhere` and `bHasFreeSocket` describe the other connections.
    pub fn DecideRoutingActivation(
        &self,
        request: &RoutingActivationRequest,
        bIsAddressAcceptable: bool,
        bIsAddressActiveElsewhere: bool,
        bHasFreeSocket: bool,
    ) -> RoutingActivationOutcome {
        if !IsSupportedActivationType(request.m_byActivationType) {
            return RoutingActivationOutcome::DeniedUnsupportedActivationType;
        }
        if !bIsAddressAcceptable {
            return RoutingActivationOutcome::DeniedUnknownSourceAddress;
        }

        // This socket already has an address registered.
        if let Some(u16Registered) = self.m_optU16SourceAddress {
            if u16Registered == request.m_u16SourceAddress {
                // Re-activation with the same address on the same socket: accepted, not an
                // error. Rejecting this is a listed trap.
                return RoutingActivationOutcome::Activated;
            }
            return RoutingActivationOutcome::DeniedSourceAddressMismatch;
        }

        // The address is live on another socket that answered an alive check.
        if bIsAddressActiveElsewhere {
            return RoutingActivationOutcome::DeniedSourceAddressInUse;
        }
        if !bHasFreeSocket {
            return RoutingActivationOutcome::DeniedAllSocketsRegistered;
        }

        RoutingActivationOutcome::Activated
    }

    /// Apply a decided outcome to this connection.
    ///
    /// The initial inactivity timer stops on **receipt of a valid routing activation request**
    /// (REQ 3.DoIP-085 NL), not on transmission of a positive response — which is why a
    /// connection parked in `PendingAuthentication` moves off the 2-second deadline here even
    /// though routing is not active. Stopping it on the response instead kills those sockets at
    /// exactly two seconds, and that is a listed trap.
    pub fn ApplyRoutingActivation(
        &mut self,
        request: &RoutingActivationRequest,
        outcome: RoutingActivationOutcome,
    ) {
        self.NoteActivity();

        match outcome {
            RoutingActivationOutcome::Activated => {
                self.m_optU16SourceAddress = Some(request.m_u16SourceAddress);
                self.m_state = ConnectionState::RoutingActive;
            }
            RoutingActivationOutcome::DeniedMissingAuthentication => {
                // Registered but not routing: the entry stays so authentication can proceed on
                // this same socket. The only denial that does not close.
                self.m_optU16SourceAddress = Some(request.m_u16SourceAddress);
                self.m_state = ConnectionState::PendingAuthentication;
            }
            _ => self.m_state = ConnectionState::Finalize,
        }
    }

    /// Mark this connection finished, so the caller closes the socket.
    pub fn Finalize(&mut self) {
        self.m_state = ConnectionState::Finalize;
    }
}

/// Whether this entity supports a routing activation type (Table 47).
///
/// `0x00` default and `0x01` required-by-regulation are mandatory; `0x02`–`0xDF` are reserved
/// by ISO and get `0x06`. The manufacturer-specific range above `0xE0` is declined here rather
/// than pretended: answering `0x10` to an activation type whose meaning we have not implemented
/// would tell a tester that something is set up when nothing is.
pub fn IsSupportedActivationType(byActivationType: u8) -> bool {
    matches!(byActivationType, 0x00 | 0x01)
}
