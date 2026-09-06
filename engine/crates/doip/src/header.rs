//! The DoIP generic header: eight bytes in front of every message.
//!
//! ISO 13400-2:2019 clause 9.2, Table 16. Every multi-byte field is big-endian network byte
//! order (REQ 8.DoIP-147 APP, clause 7.2).

use crate::payload::PayloadType;

/// Length of the generic header. The header's own length field excludes these bytes.
pub const c_uHeaderLength: usize = 8;

/// The version this implementation speaks: ISO 13400-2:2019.
pub const c_byProtocolVersion2019: u8 = 0x03;

/// ISO 13400-2:2012.
pub const c_byProtocolVersion2012: u8 = 0x02;

/// ISO/DIS 13400-2:2010.
pub const c_byProtocolVersion2010: u8 = 0x01;

/// The version a tester may use in a vehicle identification request before it knows what the
/// vehicle speaks (Table 16). Valid *only* for those requests.
pub const c_byProtocolVersionDefault: u8 = 0xFF;

/// The generic header of one DoIP message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericHeader {
    /// Protocol version byte as it arrived.
    pub m_byProtocolVersion: u8,
    /// The payload type, decoded. Unknown values are kept so a NACK can name them.
    pub m_payloadType: PayloadType,
    /// Length of the payload that follows, in bytes. Excludes the header itself.
    pub m_u32PayloadLength: u32,
}

/// Why a generic header was rejected (Table 19, clause 9.3).
///
/// The variants are exactly the NACK codes, because the whole point of this type is to carry
/// the byte that goes back on the wire — inventing a richer error here and mapping it later is
/// how the mapping goes wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderNack {
    /// `0x00` — the version and its inverse do not form the Table 16 synchronisation pattern.
    /// Send the NACK and **close the socket** (REQ 7.DoIP-041 AL).
    #[error("incorrect pattern format: version 0x{byVersion:02X} with inverse 0x{byInverse:02X}")]
    IncorrectPatternFormat { byVersion: u8, byInverse: u8 },

    /// `0x01` — this entity does not implement that payload type. NACK and **discard**
    /// (REQ 7.DoIP-042 AL).
    #[error("unknown payload type 0x{u16PayloadType:04X}")]
    UnknownPayloadType { u16PayloadType: u16 },

    /// `0x02` — the payload is longer than this entity's maximum data size, *regardless of how
    /// much memory happens to be free*. NACK and **discard** (REQ 7.DoIP-043 AL).
    #[error("payload length {u32Length} exceeds the maximum data size {u32MaxDataSize}")]
    MessageTooLarge { u32Length: u32, u32MaxDataSize: u32 },

    /// `0x03` — the payload will not fit in the memory currently available. NACK and
    /// **discard** (REQ 7.DoIP-044 AL). Distinct from `0x02`: this one is transient.
    #[error("payload length {u32Length} exceeds the memory currently available")]
    OutOfMemory { u32Length: u32 },

    /// `0x04` — the payload length is not one this payload type can have. NACK and **close the
    /// socket** (REQ 7.DoIP-045 AL).
    #[error("payload type 0x{u16PayloadType:04X} cannot have a payload of {u32Length} bytes")]
    InvalidPayloadLength { u16PayloadType: u16, u32Length: u32 },
}

impl HeaderNack {
    /// The byte that goes in the NACK payload (Table 18).
    pub fn Code(self) -> u8 {
        match self {
            HeaderNack::IncorrectPatternFormat { .. } => 0x00,
            HeaderNack::UnknownPayloadType { .. } => 0x01,
            HeaderNack::MessageTooLarge { .. } => 0x02,
            HeaderNack::OutOfMemory { .. } => 0x03,
            HeaderNack::InvalidPayloadLength { .. } => 0x04,
        }
    }

    /// Whether the socket must be closed after sending this NACK.
    ///
    /// Only `0x00` and `0x04` close (REQ 7.DoIP-041 / 7.DoIP-045 AL); the other three discard
    /// the message and keep the connection. Getting this pairing backwards is one of the
    /// failures a conformance test looks for, so it lives here next to the code rather than in
    /// whatever code happens to be handling the error.
    pub fn ClosesSocket(self) -> bool {
        matches!(
            self,
            HeaderNack::IncorrectPatternFormat { .. } | HeaderNack::InvalidPayloadLength { .. }
        )
    }
}

/// What limits this entity places on an incoming message.
#[derive(Debug, Clone, Copy)]
pub struct HeaderLimits {
    /// The entity's maximum data size — a fixed capability, reported in the entity status
    /// response and used for NACK `0x02`. The standard leaves the value to the manufacturer.
    pub m_u32MaxDataSize: u32,
    /// How much the handler can actually accept right now, for NACK `0x03`. Equal to the
    /// maximum when nothing is constraining it.
    pub m_u32AvailableMemory: u32,
}

impl Default for HeaderLimits {
    fn default() -> Self {
        // 4 kB is comfortably above any UDS message that fits ISO-TP's 4095-byte limit, so a
        // request that could be routed onto CAN can never be refused by this check.
        HeaderLimits {
            m_u32MaxDataSize: 4096,
            m_u32AvailableMemory: 4096,
        }
    }
}

/// Read and validate a generic header.
///
/// The checks run in the order ISO 13400-2 Figure 16 lays out, and the order matters for one
/// concrete reason: the length-versus-capacity check has to be decidable from these eight bytes
/// alone. A header claiming four gigabytes must be refused before anything tries to hold the
/// body.
///
/// `bIsHeaderOnly` says whether the caller has the payload yet. On UDP the whole datagram
/// arrives at once; on TCP the header is read first and the body follows, which is why the
/// length check cannot wait for it.
pub fn ReadHeader(arrBytes: &[u8], limits: HeaderLimits) -> Result<GenericHeader, HeaderNack> {
    if arrBytes.len() < c_uHeaderLength {
        // Too short to even carry a version pair; nothing else can be said about it.
        return Err(HeaderNack::IncorrectPatternFormat {
            byVersion: arrBytes.first().copied().unwrap_or(0),
            byInverse: arrBytes.get(1).copied().unwrap_or(0),
        });
    }

    let byVersion = arrBytes[0];
    let byInverse = arrBytes[1];
    if byInverse != !byVersion {
        return Err(HeaderNack::IncorrectPatternFormat {
            byVersion,
            byInverse,
        });
    }

    let u16PayloadType = u16::from_be_bytes([arrBytes[2], arrBytes[3]]);
    let u32PayloadLength = u32::from_be_bytes([arrBytes[4], arrBytes[5], arrBytes[6], arrBytes[7]]);

    let payloadType = match PayloadType::FromCode(u16PayloadType) {
        Some(payloadType) => payloadType,
        None => return Err(HeaderNack::UnknownPayloadType { u16PayloadType }),
    };

    if u32PayloadLength > limits.m_u32MaxDataSize {
        return Err(HeaderNack::MessageTooLarge {
            u32Length: u32PayloadLength,
            u32MaxDataSize: limits.m_u32MaxDataSize,
        });
    }
    if u32PayloadLength > limits.m_u32AvailableMemory {
        return Err(HeaderNack::OutOfMemory {
            u32Length: u32PayloadLength,
        });
    }

    if !payloadType.AcceptsPayloadLength(u32PayloadLength) {
        return Err(HeaderNack::InvalidPayloadLength {
            u16PayloadType,
            u32Length: u32PayloadLength,
        });
    }

    Ok(GenericHeader {
        m_byProtocolVersion: byVersion,
        m_payloadType: payloadType,
        m_u32PayloadLength: u32PayloadLength,
    })
}

/// Build a message: the generic header followed by its payload.
pub fn WriteMessage(byProtocolVersion: u8, payloadType: PayloadType, vecPayload: &[u8]) -> Vec<u8> {
    let mut vecMessage = Vec::with_capacity(c_uHeaderLength + vecPayload.len());
    vecMessage.push(byProtocolVersion);
    vecMessage.push(!byProtocolVersion);
    vecMessage.extend_from_slice(&payloadType.Code().to_be_bytes());
    vecMessage.extend_from_slice(&(vecPayload.len() as u32).to_be_bytes());
    vecMessage.extend_from_slice(vecPayload);
    vecMessage
}

/// The version to answer a peer in.
///
/// Answering in the version received is what keeps a 2012-era tester working; the standard does
/// not actually specify the reply version. The one exception is the placeholder `0xFF`, which a
/// tester may only send in a vehicle identification request and which no entity should ever
/// echo — the answer to that is this entity's real version.
pub fn ReplyVersionFor(byRequestVersion: u8) -> u8 {
    match byRequestVersion {
        c_byProtocolVersion2010 | c_byProtocolVersion2012 | c_byProtocolVersion2019 => {
            byRequestVersion
        }
        _ => c_byProtocolVersion2019,
    }
}
