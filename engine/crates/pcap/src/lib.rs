//! Reading capture files: pcap and pcapng, down to TCP and UDP payloads.
//!
//! Pure parsing with no idea what the payloads mean — the same separation `slcan` and `isotp`
//! keep. A capture is bytes on disk; turning those into diagnostic exchanges is somebody else's
//! job, and keeping the two apart means the file formats can be tested exhaustively without a
//! single diagnostic message in sight.
//!
//! Written by hand rather than pulled from a dependency. The subset needed here is small — two
//! container formats, Ethernet, IPv4/IPv6, TCP and UDP headers — and every general-purpose
//! packet library brings far more surface than that.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod classic;
pub mod ethernet;
pub mod pcapng;

use ethernet::TransportKind;

/// One packet's transport payload, with enough context to group it into a conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedPacket {
    /// Capture time in seconds. Whole seconds plus the sub-second field the file carried, at
    /// whatever resolution it declared.
    pub m_f64TimestampSec: f64,
    /// Which transport carried it.
    pub m_transport: TransportKind,
    /// Source and destination addresses, rendered for display and for grouping.
    pub m_strSourceIp: String,
    pub m_strDestinationIp: String,
    pub m_u16SourcePort: u16,
    pub m_u16DestinationPort: u16,
    /// The TCP sequence number of the first payload byte. Zero for UDP, which has none — and
    /// which needs none, because a UDP datagram is already a whole message.
    pub m_u32SequenceNumber: u32,
    /// The bytes above the transport header.
    pub m_vecPayload: Vec<u8>,
}

impl CapturedPacket {
    /// The conversation this packet belongs to: both endpoints and the transport.
    ///
    /// Directional on purpose — a TCP stream is reassembled per direction, since each carries
    /// its own independent sequence numbering.
    pub fn FlowKey(&self) -> String {
        format!(
            "{:?} {}:{} -> {}:{}",
            self.m_transport,
            self.m_strSourceIp,
            self.m_u16SourcePort,
            self.m_strDestinationIp,
            self.m_u16DestinationPort
        )
    }

    /// True when either end of this packet used the given port.
    pub fn TouchesPort(&self, u16Port: u16) -> bool {
        self.m_u16SourcePort == u16Port || self.m_u16DestinationPort == u16Port
    }
}

/// Why a capture could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PcapError {
    /// The first bytes are neither a pcap magic nor a pcapng section header.
    #[error("this is not a pcap or pcapng capture (leading bytes {strLeadingBytes})")]
    NotACapture { strLeadingBytes: String },

    /// The file ends in the middle of a structure.
    #[error("the capture is truncated: {strWhere}")]
    Truncated { strWhere: String },

    /// The link layer is not one this reader understands.
    ///
    /// Named rather than skipped. A CAN capture taken with SocketCAN is a perfectly good file
    /// that simply is not Ethernet, and telling someone that is far more use than handing back
    /// an empty result they have to explain to themselves.
    #[error("link type {u16LinkType} ({strName}) is not supported; this reader handles Ethernet")]
    UnsupportedLinkType {
        u16LinkType: u16,
        strName: &'static str,
    },

    /// A structure declares a length that cannot be right.
    #[error("the capture is malformed: {strReason}")]
    Malformed { strReason: String },
}

/// Read every TCP and UDP payload out of a capture, in file order.
///
/// Packets that are not Ethernet/IP/TCP-or-UDP are skipped rather than refused — a real capture
/// contains ARP, ICMP and whatever else was on the wire, and none of that makes the file
/// unreadable. What *is* refused is a file whose link layer means none of it could ever be
/// understood.
pub fn ReadCapture(arrBytes: &[u8]) -> Result<Vec<CapturedPacket>, PcapError> {
    if arrBytes.len() < 4 {
        return Err(PcapError::NotACapture {
            strLeadingBytes: DescribeLeadingBytes(arrBytes),
        });
    }

    let vecPackets = if pcapng::IsPcapNg(arrBytes) {
        pcapng::ReadPcapNg(arrBytes)?
    } else if classic::IsClassicPcap(arrBytes) {
        classic::ReadClassicPcap(arrBytes)?
    } else {
        return Err(PcapError::NotACapture {
            strLeadingBytes: DescribeLeadingBytes(arrBytes),
        });
    };

    tracing::info!(packets = vecPackets.len(), "read a capture");
    Ok(vecPackets)
}

/// The first few bytes in hex, so an unrecognised file can be identified from the message.
fn DescribeLeadingBytes(arrBytes: &[u8]) -> String {
    arrBytes
        .iter()
        .take(4)
        .map(|byByte| format!("{byByte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Name a link type, so an unsupported one is reported as something a person recognises.
pub fn LinkTypeName(u16LinkType: u16) -> &'static str {
    match u16LinkType {
        0 => "BSD loopback",
        1 => "Ethernet",
        101 => "raw IP",
        113 => "Linux cooked capture",
        227 => "SocketCAN",
        _ => "unknown",
    }
}
