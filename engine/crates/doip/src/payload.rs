//! DoIP payload types and their permitted lengths (ISO 13400-2:2019 Table 17).

/// Which message this is.
///
/// Only the types a vehicle entity implements are listed. Anything else decodes to `None` and
/// becomes generic header NACK `0x01` — including the optional types this entity chooses not to
/// support, which must be *refused*, not ignored (REQ 7.DoIP-042 AL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    /// `0x0000` — generic header negative acknowledge.
    GenericHeaderNack,
    /// `0x0001` — vehicle identification request.
    VehicleIdentificationRequest,
    /// `0x0002` — vehicle identification request with EID.
    VehicleIdentificationRequestByEid,
    /// `0x0003` — vehicle identification request with VIN.
    VehicleIdentificationRequestByVin,
    /// `0x0004` — vehicle announcement / vehicle identification response.
    VehicleAnnouncement,
    /// `0x0005` — routing activation request.
    RoutingActivationRequest,
    /// `0x0006` — routing activation response.
    RoutingActivationResponse,
    /// `0x0007` — alive check request.
    AliveCheckRequest,
    /// `0x0008` — alive check response.
    AliveCheckResponse,
    /// `0x4001` — DoIP entity status request.
    EntityStatusRequest,
    /// `0x4002` — DoIP entity status response.
    EntityStatusResponse,
    /// `0x4003` — diagnostic power mode information request.
    PowerModeRequest,
    /// `0x4004` — diagnostic power mode information response.
    PowerModeResponse,
    /// `0x8001` — diagnostic message.
    DiagnosticMessage,
    /// `0x8002` — diagnostic message positive acknowledgement.
    DiagnosticMessageAck,
    /// `0x8003` — diagnostic message negative acknowledgement.
    DiagnosticMessageNack,
}

impl PayloadType {
    /// The wire value.
    pub fn Code(self) -> u16 {
        match self {
            PayloadType::GenericHeaderNack => 0x0000,
            PayloadType::VehicleIdentificationRequest => 0x0001,
            PayloadType::VehicleIdentificationRequestByEid => 0x0002,
            PayloadType::VehicleIdentificationRequestByVin => 0x0003,
            PayloadType::VehicleAnnouncement => 0x0004,
            PayloadType::RoutingActivationRequest => 0x0005,
            PayloadType::RoutingActivationResponse => 0x0006,
            PayloadType::AliveCheckRequest => 0x0007,
            PayloadType::AliveCheckResponse => 0x0008,
            PayloadType::EntityStatusRequest => 0x4001,
            PayloadType::EntityStatusResponse => 0x4002,
            PayloadType::PowerModeRequest => 0x4003,
            PayloadType::PowerModeResponse => 0x4004,
            PayloadType::DiagnosticMessage => 0x8001,
            PayloadType::DiagnosticMessageAck => 0x8002,
            PayloadType::DiagnosticMessageNack => 0x8003,
        }
    }

    /// Decode a wire value, or `None` for one this entity does not implement.
    pub fn FromCode(u16Code: u16) -> Option<Self> {
        let payloadType = match u16Code {
            0x0000 => PayloadType::GenericHeaderNack,
            0x0001 => PayloadType::VehicleIdentificationRequest,
            0x0002 => PayloadType::VehicleIdentificationRequestByEid,
            0x0003 => PayloadType::VehicleIdentificationRequestByVin,
            0x0004 => PayloadType::VehicleAnnouncement,
            0x0005 => PayloadType::RoutingActivationRequest,
            0x0006 => PayloadType::RoutingActivationResponse,
            0x0007 => PayloadType::AliveCheckRequest,
            0x0008 => PayloadType::AliveCheckResponse,
            0x4001 => PayloadType::EntityStatusRequest,
            0x4002 => PayloadType::EntityStatusResponse,
            0x4003 => PayloadType::PowerModeRequest,
            0x4004 => PayloadType::PowerModeResponse,
            0x8001 => PayloadType::DiagnosticMessage,
            0x8002 => PayloadType::DiagnosticMessageAck,
            0x8003 => PayloadType::DiagnosticMessageNack,
            _ => return None,
        };
        Some(payloadType)
    }

    /// True when a payload of this length is one the type can carry.
    ///
    /// The lengths come from the message tables rather than from a list in the standard, so the
    /// optional-tail cases are spelled out: a routing activation request is 7 bytes, or 11 with
    /// the manufacturer-specific field; an announcement is 32, or 33 with the synchronisation
    /// status. Accepting only one of each pair is a real interoperability failure — both forms
    /// are conformant and testers send both.
    pub fn AcceptsPayloadLength(self, u32Length: u32) -> bool {
        match self {
            PayloadType::GenericHeaderNack => u32Length == 1,
            PayloadType::VehicleIdentificationRequest => u32Length == 0,
            PayloadType::VehicleIdentificationRequestByEid => u32Length == 6,
            PayloadType::VehicleIdentificationRequestByVin => u32Length == 17,
            PayloadType::VehicleAnnouncement => u32Length == 32 || u32Length == 33,
            PayloadType::RoutingActivationRequest => u32Length == 7 || u32Length == 11,
            PayloadType::RoutingActivationResponse => u32Length == 9 || u32Length == 13,
            PayloadType::AliveCheckRequest => u32Length == 0,
            PayloadType::AliveCheckResponse => u32Length == 2,
            PayloadType::EntityStatusRequest => u32Length == 0,
            PayloadType::EntityStatusResponse => u32Length == 3 || u32Length == 7,
            PayloadType::PowerModeRequest => u32Length == 0,
            PayloadType::PowerModeResponse => u32Length == 1,
            // Source and target address, then at least one byte of user data. Table 21 sets no
            // minimum for the user data, so an empty one is arguably legal — but a diagnostic
            // message with no service identifier cannot be routed anywhere, and refusing it
            // here gives the tester a precise answer instead of a silence.
            PayloadType::DiagnosticMessage => u32Length >= 5,
            PayloadType::DiagnosticMessageAck | PayloadType::DiagnosticMessageNack => {
                u32Length >= 5
            }
        }
    }

    /// True for a message that arrives on the UDP discovery port.
    pub fn IsUdpDiscovery(self) -> bool {
        matches!(
            self,
            PayloadType::VehicleIdentificationRequest
                | PayloadType::VehicleIdentificationRequestByEid
                | PayloadType::VehicleIdentificationRequestByVin
                | PayloadType::EntityStatusRequest
                | PayloadType::PowerModeRequest
        )
    }

    /// True for a vehicle identification request in any of its three forms.
    ///
    /// These are the messages whose protocol version byte must be ignored outright
    /// (REQ 7.DoIP-156 AL) — a tester that has not yet discovered the vehicle cannot know what
    /// version to use, which is what the `0xFF` placeholder is for.
    pub fn IsVehicleIdentificationRequest(self) -> bool {
        matches!(
            self,
            PayloadType::VehicleIdentificationRequest
                | PayloadType::VehicleIdentificationRequestByEid
                | PayloadType::VehicleIdentificationRequestByVin
        )
    }
}
