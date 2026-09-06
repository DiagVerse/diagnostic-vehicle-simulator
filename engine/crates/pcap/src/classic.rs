//! Classic pcap: a 24-byte file header, then a 16-byte record header before each packet.

use crate::ethernet::ReadEthernetFrame;
use crate::{CapturedPacket, LinkTypeName, PcapError};

const c_uFileHeaderLength: usize = 24;
const c_uRecordHeaderLength: usize = 16;
const c_u16LinkTypeEthernet: u16 = 1;

/// The four magics, which encode both byte order and timestamp resolution.
const c_u32MagicMicrosecondsBigEndian: u32 = 0xA1B2_C3D4;
const c_u32MagicMicrosecondsLittleEndian: u32 = 0xD4C3_B2A1;
const c_u32MagicNanosecondsBigEndian: u32 = 0xA1B2_3C4D;
const c_u32MagicNanosecondsLittleEndian: u32 = 0x4D3C_B2A1;

/// How the rest of the file must be read.
#[derive(Debug, Clone, Copy)]
struct FileFormat {
    m_bIsLittleEndian: bool,
    /// Divisor turning the sub-second field into seconds: a million, or a billion.
    m_f64SubSecondDivisor: f64,
}

/// True when these bytes start with a classic pcap magic.
pub fn IsClassicPcap(arrBytes: &[u8]) -> bool {
    ReadFormat(arrBytes).is_some()
}

/// Read every packet from a classic pcap file.
pub fn ReadClassicPcap(arrBytes: &[u8]) -> Result<Vec<CapturedPacket>, PcapError> {
    if arrBytes.len() < c_uFileHeaderLength {
        return Err(PcapError::Truncated {
            strWhere: "the 24-byte pcap file header".to_string(),
        });
    }

    let format = ReadFormat(arrBytes).ok_or_else(|| PcapError::NotACapture {
        strLeadingBytes: "not a pcap magic".to_string(),
    })?;

    // The link type is a whole-file property, so an unsupported one is decided once here rather
    // than silently producing nothing after reading every record.
    let u32LinkType = ReadU32(arrBytes, 20, format.m_bIsLittleEndian);
    let u16LinkType = u32LinkType as u16;
    if u16LinkType != c_u16LinkTypeEthernet {
        return Err(PcapError::UnsupportedLinkType {
            u16LinkType,
            strName: LinkTypeName(u16LinkType),
        });
    }

    let mut vecPackets = Vec::new();
    let mut uOffset = c_uFileHeaderLength;

    while uOffset + c_uRecordHeaderLength <= arrBytes.len() {
        let u32Seconds = ReadU32(arrBytes, uOffset, format.m_bIsLittleEndian);
        let u32SubSeconds = ReadU32(arrBytes, uOffset + 4, format.m_bIsLittleEndian);
        let u32CapturedLength = ReadU32(arrBytes, uOffset + 8, format.m_bIsLittleEndian);

        let uStart = uOffset + c_uRecordHeaderLength;
        let uEnd = uStart.saturating_add(u32CapturedLength as usize);
        if uEnd > arrBytes.len() {
            // A capture cut short mid-packet is common — a tool killed while writing. Everything
            // read so far is still good, so it is returned with a warning rather than thrown away.
            tracing::warn!(
                at = uOffset,
                declared = u32CapturedLength,
                available = arrBytes.len() - uStart,
                "the capture ends mid-packet; returning what was read"
            );
            break;
        }

        let f64TimestampSec =
            u32Seconds as f64 + (u32SubSeconds as f64 / format.m_f64SubSecondDivisor);
        if let Some(packet) = ReadEthernetFrame(&arrBytes[uStart..uEnd], f64TimestampSec)? {
            vecPackets.push(packet);
        }

        uOffset = uEnd;
    }

    Ok(vecPackets)
}

/// Decide byte order and timestamp resolution from the magic.
fn ReadFormat(arrBytes: &[u8]) -> Option<FileFormat> {
    if arrBytes.len() < 4 {
        return None;
    }
    let u32BigEndian = u32::from_be_bytes([arrBytes[0], arrBytes[1], arrBytes[2], arrBytes[3]]);

    match u32BigEndian {
        c_u32MagicMicrosecondsBigEndian => Some(FileFormat {
            m_bIsLittleEndian: false,
            m_f64SubSecondDivisor: 1_000_000.0,
        }),
        c_u32MagicMicrosecondsLittleEndian => Some(FileFormat {
            m_bIsLittleEndian: true,
            m_f64SubSecondDivisor: 1_000_000.0,
        }),
        c_u32MagicNanosecondsBigEndian => Some(FileFormat {
            m_bIsLittleEndian: false,
            m_f64SubSecondDivisor: 1_000_000_000.0,
        }),
        c_u32MagicNanosecondsLittleEndian => Some(FileFormat {
            m_bIsLittleEndian: true,
            m_f64SubSecondDivisor: 1_000_000_000.0,
        }),
        _ => None,
    }
}

/// Read a 32-bit field in the file's byte order.
fn ReadU32(arrBytes: &[u8], uOffset: usize, bIsLittleEndian: bool) -> u32 {
    let arrField = [
        arrBytes[uOffset],
        arrBytes[uOffset + 1],
        arrBytes[uOffset + 2],
        arrBytes[uOffset + 3],
    ];
    if bIsLittleEndian {
        u32::from_le_bytes(arrField)
    } else {
        u32::from_be_bytes(arrField)
    }
}
