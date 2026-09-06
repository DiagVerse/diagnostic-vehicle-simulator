//! Ethernet, IPv4/IPv6, TCP and UDP — enough header parsing to reach a payload.

use crate::{CapturedPacket, PcapError};

/// Which transport carried a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Tcp,
    Udp,
}

const c_uEthernetHeaderLength: usize = 14;
const c_u16EtherTypeIpv4: u16 = 0x0800;
const c_u16EtherTypeIpv6: u16 = 0x86DD;
const c_u16EtherTypeVlan: u16 = 0x8100;
const c_u16EtherTypeVlanStacked: u16 = 0x88A8;
const c_uVlanTagLength: usize = 4;

const c_byIpProtocolTcp: u8 = 6;
const c_byIpProtocolUdp: u8 = 17;
const c_uIpv6HeaderLength: usize = 40;

/// Pull the transport payload out of one Ethernet frame.
///
/// `Ok(None)` for a frame this reader has no interest in — ARP, ICMP, a protocol above IP that
/// is neither TCP nor UDP. That is not an error: a real capture is full of other traffic, and
/// refusing the file because of it would make captures taken on a live network unusable.
pub fn ReadEthernetFrame(
    arrFrame: &[u8],
    f64TimestampSec: f64,
) -> Result<Option<CapturedPacket>, PcapError> {
    if arrFrame.len() < c_uEthernetHeaderLength {
        return Ok(None);
    }

    // Walk past any VLAN tags. A tagged frame is an ordinary frame wearing a hat, and refusing
    // to look under it would silently lose every packet on a trunk port.
    let mut uOffset = 12;
    let mut u16EtherType = ReadU16(arrFrame, uOffset)?;
    uOffset += 2;

    while u16EtherType == c_u16EtherTypeVlan || u16EtherType == c_u16EtherTypeVlanStacked {
        if arrFrame.len() < uOffset + c_uVlanTagLength {
            return Ok(None);
        }
        uOffset += 2; // the tag control information
        u16EtherType = ReadU16(arrFrame, uOffset)?;
        uOffset += 2;
    }

    match u16EtherType {
        c_u16EtherTypeIpv4 => ReadIpv4(arrFrame, uOffset, f64TimestampSec),
        c_u16EtherTypeIpv6 => ReadIpv6(arrFrame, uOffset, f64TimestampSec),
        _ => Ok(None),
    }
}

/// IPv4: variable header length, and fragments this reader will not guess at.
fn ReadIpv4(
    arrFrame: &[u8],
    uOffset: usize,
    f64TimestampSec: f64,
) -> Result<Option<CapturedPacket>, PcapError> {
    if arrFrame.len() < uOffset + 20 {
        return Ok(None);
    }

    let uHeaderLength = ((arrFrame[uOffset] & 0x0F) as usize) * 4;
    if uHeaderLength < 20 || arrFrame.len() < uOffset + uHeaderLength {
        return Ok(None);
    }

    // A fragmented datagram carries only part of its transport payload, and reassembling IP
    // fragments is a different job with its own failure modes. Skipping is honest; parsing the
    // first fragment as though it were whole would produce a plausible, truncated PDU.
    let u16FragmentField = ReadU16(arrFrame, uOffset + 6)?;
    let bIsFragment = (u16FragmentField & 0x1FFF) != 0 || (u16FragmentField & 0x2000) != 0;
    if bIsFragment {
        tracing::debug!("skipping a fragmented IPv4 datagram; reassembly is not implemented");
        return Ok(None);
    }

    // The total length field bounds the payload: an Ethernet frame may be padded to its 60-byte
    // minimum, and that padding is not data.
    let uTotalLength = ReadU16(arrFrame, uOffset + 2)? as usize;
    let uAvailable = arrFrame.len() - uOffset;
    let uDatagramLength = uTotalLength.clamp(uHeaderLength, uAvailable);

    let byProtocol = arrFrame[uOffset + 9];
    let strSource = FormatIpv4(&arrFrame[uOffset + 12..uOffset + 16]);
    let strDestination = FormatIpv4(&arrFrame[uOffset + 16..uOffset + 20]);

    ReadTransport(
        &arrFrame[uOffset + uHeaderLength..uOffset + uDatagramLength],
        byProtocol,
        strSource,
        strDestination,
        f64TimestampSec,
    )
}

/// IPv6: a fixed header, and extension headers this reader does not walk.
fn ReadIpv6(
    arrFrame: &[u8],
    uOffset: usize,
    f64TimestampSec: f64,
) -> Result<Option<CapturedPacket>, PcapError> {
    if arrFrame.len() < uOffset + c_uIpv6HeaderLength {
        return Ok(None);
    }

    let byNextHeader = arrFrame[uOffset + 6];
    // Extension headers would need a chain walk. Diagnostic traffic does not use them, and
    // guessing at an offset would mis-parse rather than fail.
    if byNextHeader != c_byIpProtocolTcp && byNextHeader != c_byIpProtocolUdp {
        return Ok(None);
    }

    let uPayloadLength = ReadU16(arrFrame, uOffset + 4)? as usize;
    let uStart = uOffset + c_uIpv6HeaderLength;
    let uAvailable = arrFrame.len() - uStart;
    let uEnd = uStart + uPayloadLength.min(uAvailable);

    let strSource = FormatIpv6(&arrFrame[uOffset + 8..uOffset + 24]);
    let strDestination = FormatIpv6(&arrFrame[uOffset + 24..uOffset + 40]);

    ReadTransport(
        &arrFrame[uStart..uEnd],
        byNextHeader,
        strSource,
        strDestination,
        f64TimestampSec,
    )
}

/// TCP or UDP, down to the payload.
fn ReadTransport(
    arrSegment: &[u8],
    byProtocol: u8,
    strSourceIp: String,
    strDestinationIp: String,
    f64TimestampSec: f64,
) -> Result<Option<CapturedPacket>, PcapError> {
    match byProtocol {
        c_byIpProtocolTcp => {
            if arrSegment.len() < 20 {
                return Ok(None);
            }
            let uDataOffset = ((arrSegment[12] >> 4) as usize) * 4;
            if uDataOffset < 20 || arrSegment.len() < uDataOffset {
                return Ok(None);
            }

            Ok(Some(CapturedPacket {
                // Filled in by `ReadCapture` once the whole file has been read.
                m_uCaptureIndex: 0,
                m_f64TimestampSec: f64TimestampSec,
                m_transport: TransportKind::Tcp,
                m_strSourceIp: strSourceIp,
                m_strDestinationIp: strDestinationIp,
                m_u16SourcePort: ReadU16(arrSegment, 0)?,
                m_u16DestinationPort: ReadU16(arrSegment, 2)?,
                m_u32SequenceNumber: ReadU32(arrSegment, 4)?,
                m_vecPayload: arrSegment[uDataOffset..].to_vec(),
            }))
        }

        c_byIpProtocolUdp => {
            if arrSegment.len() < 8 {
                return Ok(None);
            }
            // The UDP length field covers the header too, and bounds the payload against any
            // trailing padding the frame carried.
            let uLength = ReadU16(arrSegment, 4)? as usize;
            let uEnd = uLength.clamp(8, arrSegment.len());

            Ok(Some(CapturedPacket {
                // Filled in by `ReadCapture` once the whole file has been read.
                m_uCaptureIndex: 0,
                m_f64TimestampSec: f64TimestampSec,
                m_transport: TransportKind::Udp,
                m_strSourceIp: strSourceIp,
                m_strDestinationIp: strDestinationIp,
                m_u16SourcePort: ReadU16(arrSegment, 0)?,
                m_u16DestinationPort: ReadU16(arrSegment, 2)?,
                m_u32SequenceNumber: 0,
                m_vecPayload: arrSegment[8..uEnd].to_vec(),
            }))
        }

        _ => Ok(None),
    }
}

fn ReadU16(arrBytes: &[u8], uOffset: usize) -> Result<u16, PcapError> {
    if arrBytes.len() < uOffset + 2 {
        return Err(PcapError::Truncated {
            strWhere: format!("reading two bytes at offset {uOffset}"),
        });
    }
    Ok(u16::from_be_bytes([
        arrBytes[uOffset],
        arrBytes[uOffset + 1],
    ]))
}

fn ReadU32(arrBytes: &[u8], uOffset: usize) -> Result<u32, PcapError> {
    if arrBytes.len() < uOffset + 4 {
        return Err(PcapError::Truncated {
            strWhere: format!("reading four bytes at offset {uOffset}"),
        });
    }
    Ok(u32::from_be_bytes([
        arrBytes[uOffset],
        arrBytes[uOffset + 1],
        arrBytes[uOffset + 2],
        arrBytes[uOffset + 3],
    ]))
}

fn FormatIpv4(arrAddress: &[u8]) -> String {
    format!(
        "{}.{}.{}.{}",
        arrAddress[0], arrAddress[1], arrAddress[2], arrAddress[3]
    )
}

/// IPv6 in the conventional colon-separated form, without the `::` run compression — this is a
/// label for grouping and display, not something anybody parses back.
fn FormatIpv6(arrAddress: &[u8]) -> String {
    (0..8)
        .map(|uGroup| {
            let u16Group = u16::from_be_bytes([arrAddress[uGroup * 2], arrAddress[uGroup * 2 + 1]]);
            format!("{u16Group:x}")
        })
        .collect::<Vec<_>>()
        .join(":")
}
