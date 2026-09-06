//! What this DoIP entity says about itself, and what it can be made to say instead.
//!
//! Two kinds of knob, deliberately in one place because a reader wants to see them together:
//! the entity's genuine parameters (power mode, socket capacity, maximum data size), and the
//! fault injection that makes it answer something a healthy entity would not.
//!
//! Everything here defaults to the behaviour the entity had before it was configurable, so an
//! untouched simulation behaves exactly as it did.

use doip::messages::{c_byNodeTypeGateway, c_byPowerModeReady};

/// How many concurrent TCP data sockets the entity serves by default.
///
/// ISO 13400-2 names no value — it is manufacturer discretion — and requires `<n+1>` resources
/// so one is always free for socket handling. This is the `<n>` reported to a tester.
pub const c_uDefaultMaxConnections: u8 = 4;

/// The entity's maximum data size, reported in the entity status response and used for the
/// generic header's "message too large" check.
pub const c_u32DefaultMaxDataSize: u32 = 4096;

/// The parameters and fault injection of one DoIP entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoIpSettings {
    /// Diagnostic power mode (clause 7.5): `0x00` not ready, `0x01` ready, `0x02` not supported.
    ///
    /// A tester is entitled to refuse to proceed when this is not `0x01`, so being able to say
    /// "not ready" is how that path gets exercised.
    pub m_byPowerMode: u8,

    /// Entity status node type: `0x00` gateway, `0x01` node.
    pub m_byNodeType: u8,

    /// Concurrent TCP data sockets reported to a tester, **excluding** the reserve socket the
    /// standard requires for socket handling.
    pub m_byMaxSockets: u8,

    /// The maximum data size reported, and enforced by the header check.
    ///
    /// The two must agree: a tester that reads this and then sends a message that size expects
    /// it to be accepted, and a conformance test cross-checks them.
    pub m_u32MaxDataSize: u32,

    /// Answer nothing to a vehicle identification request.
    ///
    /// A vehicle that cannot be discovered is a real failure to reproduce — an entity still
    /// booting, or one on a segment the tester cannot reach — and a tester's handling of it is
    /// worth exercising.
    pub m_bSuppressIdentificationResponse: bool,

    /// Answer every routing activation with this response code instead of deciding.
    ///
    /// `Some(0x00)` refuses an unknown source address, `Some(0x06)` an unsupported activation
    /// type, and so on. `None` lets the connection state machine decide, which is the normal
    /// case.
    pub m_optByForcedRoutingActivationCode: Option<u8>,

    /// Negatively acknowledge every diagnostic message with this code instead of routing it.
    ///
    /// `Some(0x03)` makes every target look unknown; `Some(0x04)` makes every message look too
    /// large. Note that `0x02` closes the socket, which is itself worth being able to provoke.
    pub m_optByForcedDiagnosticNack: Option<u8>,

    /// Refuse every message with this generic header NACK code, before it is even dispatched.
    ///
    /// The bluntest of the three, and the one that exercises a tester's handling of an entity
    /// that has stopped making sense. `0x00` and `0x04` also close the socket.
    pub m_optByForcedHeaderNack: Option<u8>,
}

impl Default for DoIpSettings {
    fn default() -> Self {
        DoIpSettings {
            m_byPowerMode: c_byPowerModeReady,
            m_byNodeType: c_byNodeTypeGateway,
            m_byMaxSockets: c_uDefaultMaxConnections,
            m_u32MaxDataSize: c_u32DefaultMaxDataSize,
            m_bSuppressIdentificationResponse: false,
            m_optByForcedRoutingActivationCode: None,
            m_optByForcedDiagnosticNack: None,
            m_optByForcedHeaderNack: None,
        }
    }
}

impl DoIpSettings {
    /// True when nothing is being injected and the entity behaves as a healthy one.
    ///
    /// Used to say so on screen: an entity quietly refusing everything because a setting was
    /// left on is a confusing afternoon, and the UI should be able to warn.
    pub fn IsInjectingFaults(&self) -> bool {
        self.m_bSuppressIdentificationResponse
            || self.m_optByForcedRoutingActivationCode.is_some()
            || self.m_optByForcedDiagnosticNack.is_some()
            || self.m_optByForcedHeaderNack.is_some()
    }
}
