//! The payloads themselves: what each DoIP message carries once the header is off.

use crate::payload::PayloadType;

/// The vehicle announcement / vehicle identification response (Table 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleAnnouncement {
    /// Seventeen bytes. Use ISO 13400-2 Table 1's invalidity fill when not programmed.
    pub m_arrVin: [u8; 17],
    /// The logical address of the entity that is answering.
    pub m_u16LogicalAddress: u16,
    /// Entity identification — six bytes, conventionally the MAC.
    pub m_arrEid: [u8; 6],
    /// Group identification — six bytes shared by every entity of one vehicle.
    pub m_arrGid: [u8; 6],
    /// Table 6. `0x00` no further action; `0x10` routing activation required for central security.
    pub m_byFurtherActionRequired: u8,
    /// Table 7. `0x00` synchronized, `0x10` not — and optional, hence the `Option`.
    ///
    /// Left out the payload is 32 bytes rather than 33, and both are conformant. Modelling it
    /// as absent rather than as a defaulted zero keeps that distinction, because a tester can
    /// see the difference and some behave differently.
    pub m_optBySyncStatus: Option<u8>,
}

impl VehicleAnnouncement {
    /// Serialize the payload.
    pub fn ToBytes(&self) -> Vec<u8> {
        let mut vecPayload = Vec::with_capacity(33);
        vecPayload.extend_from_slice(&self.m_arrVin);
        vecPayload.extend_from_slice(&self.m_u16LogicalAddress.to_be_bytes());
        vecPayload.extend_from_slice(&self.m_arrEid);
        vecPayload.extend_from_slice(&self.m_arrGid);
        vecPayload.push(self.m_byFurtherActionRequired);
        if let Some(bySyncStatus) = self.m_optBySyncStatus {
            vecPayload.push(bySyncStatus);
        }
        vecPayload
    }
}

/// The routing activation request (Table 46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingActivationRequest {
    /// The tester's logical address — and the source address it will use in every diagnostic
    /// message on this socket from now on.
    pub m_u16SourceAddress: u16,
    /// Table 47. `0x00` default, `0x01` required by regulation, `0xE0` central security.
    pub m_byActivationType: u8,
    /// Four bytes reserved by ISO; `0x00000000` in practice.
    pub m_u32ReservedIso: u32,
    /// The optional manufacturer-specific tail, present when the payload is 11 bytes.
    pub m_optU32ReservedOem: Option<u32>,
}

impl RoutingActivationRequest {
    /// Read the payload. `None` when it is not one of the two conformant lengths — though the
    /// header check has already refused those, so this is belt and braces.
    pub fn FromBytes(vecPayload: &[u8]) -> Option<Self> {
        if vecPayload.len() != 7 && vecPayload.len() != 11 {
            return None;
        }
        Some(RoutingActivationRequest {
            m_u16SourceAddress: u16::from_be_bytes([vecPayload[0], vecPayload[1]]),
            m_byActivationType: vecPayload[2],
            m_u32ReservedIso: u32::from_be_bytes([
                vecPayload[3],
                vecPayload[4],
                vecPayload[5],
                vecPayload[6],
            ]),
            m_optU32ReservedOem: (vecPayload.len() == 11).then(|| {
                u32::from_be_bytes([vecPayload[7], vecPayload[8], vecPayload[9], vecPayload[10]])
            }),
        })
    }
}

/// What the entity decided about a routing activation request (Table 49).
///
/// The variants carry no data because the response code *is* the whole answer; what differs
/// between them is what the server must then do, which `IsActivated` and `ClosesSocket` state
/// rather than leaving to whoever handles the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingActivationOutcome {
    /// `0x00` — the source address is not one this entity accepts.
    DeniedUnknownSourceAddress,
    /// `0x01` — every socket is registered and active, and all of them answered an alive check.
    DeniedAllSocketsRegistered,
    /// `0x02` — this socket is already activated with a *different* source address.
    DeniedSourceAddressMismatch,
    /// `0x03` — that source address is already active on another socket, which is still alive.
    DeniedSourceAddressInUse,
    /// `0x04` — authentication is required and has not happened. The **only** denial that
    /// leaves the socket open (Table 49: "do not activate routing and register").
    DeniedMissingAuthentication,
    /// `0x06` — the activation type is not one this entity supports.
    DeniedUnsupportedActivationType,
    /// `0x10` — routing is active.
    Activated,
}

impl RoutingActivationOutcome {
    /// The response code byte.
    pub fn Code(self) -> u8 {
        match self {
            RoutingActivationOutcome::DeniedUnknownSourceAddress => 0x00,
            RoutingActivationOutcome::DeniedAllSocketsRegistered => 0x01,
            RoutingActivationOutcome::DeniedSourceAddressMismatch => 0x02,
            RoutingActivationOutcome::DeniedSourceAddressInUse => 0x03,
            RoutingActivationOutcome::DeniedMissingAuthentication => 0x04,
            RoutingActivationOutcome::DeniedUnsupportedActivationType => 0x06,
            RoutingActivationOutcome::Activated => 0x10,
        }
    }

    /// True when routing is now active on this socket.
    pub fn IsActivated(self) -> bool {
        matches!(self, RoutingActivationOutcome::Activated)
    }

    /// Whether the socket must be closed after the response goes out.
    ///
    /// Every denial closes except `0x04`, whose required action is literally "do not activate
    /// routing **and register**" — the connection-table entry stays so authentication can
    /// proceed on the same socket. That single exception is easy to miss and easy to get
    /// backwards, so it is stated here rather than at the call site.
    pub fn ClosesSocket(self) -> bool {
        !matches!(
            self,
            RoutingActivationOutcome::Activated
                | RoutingActivationOutcome::DeniedMissingAuthentication
        )
    }
}

/// Build a routing activation response payload (Table 48).
pub fn BuildRoutingActivationResponse(
    u16TesterAddress: u16,
    u16EntityAddress: u16,
    outcome: RoutingActivationOutcome,
) -> Vec<u8> {
    let mut vecPayload = Vec::with_capacity(9);
    vecPayload.extend_from_slice(&u16TesterAddress.to_be_bytes());
    vecPayload.extend_from_slice(&u16EntityAddress.to_be_bytes());
    vecPayload.push(outcome.Code());
    vecPayload.extend_from_slice(&0u32.to_be_bytes());
    vecPayload
}

/// A diagnostic message (Table 21): the UDS payload with its addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticMessage {
    /// The sender's logical address.
    pub m_u16SourceAddress: u16,
    /// The intended receiver's logical address.
    pub m_u16TargetAddress: u16,
    /// The ISO 14229-1 payload. DoIP replaces ISO-TP, not the diagnostic layer, so there is no
    /// segmentation here — TCP delivers the whole message and the header's length bounds it.
    pub m_vecUserData: Vec<u8>,
}

impl DiagnosticMessage {
    /// Read the payload.
    pub fn FromBytes(vecPayload: &[u8]) -> Option<Self> {
        if vecPayload.len() < 5 {
            return None;
        }
        Some(DiagnosticMessage {
            m_u16SourceAddress: u16::from_be_bytes([vecPayload[0], vecPayload[1]]),
            m_u16TargetAddress: u16::from_be_bytes([vecPayload[2], vecPayload[3]]),
            m_vecUserData: vecPayload[4..].to_vec(),
        })
    }

    /// Serialize the payload.
    pub fn ToBytes(&self) -> Vec<u8> {
        let mut vecPayload = Vec::with_capacity(4 + self.m_vecUserData.len());
        vecPayload.extend_from_slice(&self.m_u16SourceAddress.to_be_bytes());
        vecPayload.extend_from_slice(&self.m_u16TargetAddress.to_be_bytes());
        vecPayload.extend_from_slice(&self.m_vecUserData);
        vecPayload
    }
}

/// Why a diagnostic message could not be routed (Table 26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticNack {
    /// `0x02` — the message's source address is not the one activated on this socket.
    InvalidSourceAddress,
    /// `0x03` — no server is known at that target address.
    UnknownTargetAddress,
    /// `0x04` — too large for the target network's transport. This is what a functionally
    /// addressed message longer than a CAN SingleFrame gets.
    MessageTooLarge,
    /// `0x05` — the destination buffer could not be provided.
    OutOfMemory,
    /// `0x06` — the target exists but cannot be reached right now.
    TargetUnreachable,
}

impl DiagnosticNack {
    /// The NACK code byte.
    pub fn Code(self) -> u8 {
        match self {
            DiagnosticNack::InvalidSourceAddress => 0x02,
            DiagnosticNack::UnknownTargetAddress => 0x03,
            DiagnosticNack::MessageTooLarge => 0x04,
            DiagnosticNack::OutOfMemory => 0x05,
            DiagnosticNack::TargetUnreachable => 0x06,
        }
    }

    /// Whether the socket must be closed after this NACK.
    ///
    /// Only `0x02` (REQ 7.DoIP-070 AL) — and that rule lives in the requirement text, not in
    /// Table 26, which has no required-action column at all. Everything else discards the
    /// message and keeps the connection.
    pub fn ClosesSocket(self) -> bool {
        matches!(self, DiagnosticNack::InvalidSourceAddress)
    }
}

/// Build a diagnostic message acknowledgement or negative acknowledgement.
///
/// **The addresses are swapped relative to the message being acknowledged** (Table 23): the
/// acknowledgement's source is the intended *receiver* of that message and its target is its
/// *sender*. Echoing the original pair unchanged is the most common bug in this payload and is
/// directly visible to a conformance test, which is why this function takes the original
/// message and does the swap itself rather than trusting a caller to remember.
pub fn BuildDiagnosticAck(request: &DiagnosticMessage, byCode: u8) -> Vec<u8> {
    let mut vecPayload = Vec::with_capacity(5);
    vecPayload.extend_from_slice(&request.m_u16TargetAddress.to_be_bytes());
    vecPayload.extend_from_slice(&request.m_u16SourceAddress.to_be_bytes());
    vecPayload.push(byCode);
    vecPayload
}

/// The acknowledgement code: "routed and put into the destination transmission buffer".
///
/// `0x00` is the only value Table 24 defines. It means *routed*, never *accepted* — a later
/// negative response from the ECU does not contradict it.
pub const c_byAckRoutingConfirmation: u8 = 0x00;

/// The entity status response (Table 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityStatus {
    /// `0x00` gateway, `0x01` node.
    pub m_byNodeType: u8,
    /// How many concurrent TCP data sockets this entity supports — **excluding** the reserve
    /// socket it keeps for socket handling.
    pub m_byMaxSockets: u8,
    /// How many are open right now.
    pub m_byOpenSockets: u8,
    /// The maximum data size, when the entity reports one.
    pub m_optU32MaxDataSize: Option<u32>,
}

impl EntityStatus {
    /// Serialize the payload.
    pub fn ToBytes(&self) -> Vec<u8> {
        let mut vecPayload = Vec::with_capacity(7);
        vecPayload.push(self.m_byNodeType);
        vecPayload.push(self.m_byMaxSockets);
        vecPayload.push(self.m_byOpenSockets);
        if let Some(u32MaxDataSize) = self.m_optU32MaxDataSize {
            vecPayload.extend_from_slice(&u32MaxDataSize.to_be_bytes());
        }
        vecPayload
    }
}

/// Node type: an entity that routes to other networks.
pub const c_byNodeTypeGateway: u8 = 0x00;

/// Diagnostic power mode (clause 7.5): `0x00` not ready, `0x01` ready, `0x02` not supported.
pub const c_byPowerModeReady: u8 = 0x01;

/// The alive check response payload: the source address active on the socket (Table 28).
pub fn BuildAliveCheckResponse(u16SourceAddress: u16) -> Vec<u8> {
    u16SourceAddress.to_be_bytes().to_vec()
}

/// True for a logical address in the functional (broadcast) range.
///
/// ISO 13400-2 Table 13 gives `0xE000`–`0xEFFF` to functional group addresses. DoIP itself has
/// no multicast — the tester unicasts to the entity, and a gateway that receives a functional
/// target address is the thing that broadcasts onto its sub-networks.
pub fn IsFunctionalAddress(u16Address: u16) -> bool {
    (0xE000..=0xEFFF).contains(&u16Address)
}

/// The payload type an outcome is sent as.
pub fn AckPayloadType(bIsPositive: bool) -> PayloadType {
    if bIsPositive {
        PayloadType::DiagnosticMessageAck
    } else {
        PayloadType::DiagnosticMessageNack
    }
}
