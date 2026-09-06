//! pcapng: a stream of length-prefixed blocks, of which three matter here.
//!
//! Every block carries its own total length, which makes the format forgiving to read — a block
//! type this reader does not know is skipped by that length rather than being a parse failure.
//! Wireshark writes pcapng by default, so supporting it is not optional in practice.

use crate::ethernet::ReadEthernetFrame;
use crate::{CapturedPacket, LinkTypeName, PcapError};

/// Section Header Block — starts a file, and declares the byte order for what follows.
const c_u32BlockSectionHeader: u32 = 0x0A0D_0D0A;
/// Interface Description Block — one per capture interface, carrying the link type.
const c_u32BlockInterfaceDescription: u32 = 0x0000_0001;
/// Enhanced Packet Block — a captured packet.
const c_u32BlockEnhancedPacket: u32 = 0x0000_0006;

/// The byte-order magic inside a section header, read big-endian.
const c_u32ByteOrderMagicBigEndian: u32 = 0x1A2B_3C4D;

const c_u16LinkTypeEthernet: u16 = 1;
/// The option code carrying an interface's timestamp resolution.
const c_u16OptionTimestampResolution: u16 = 9;
/// The resolution assumed when an interface does not say: microseconds.
const c_byDefaultTimestampResolution: u8 = 6;

/// One interface, as far as reading packets needs to know.
struct Interface {
    m_u16LinkType: u16,
    /// Ticks per second, derived from the `if_tsresol` option.
    m_f64TicksPerSecond: f64,
}

/// True when these bytes start with a pcapng section header block.
pub fn IsPcapNg(arrBytes: &[u8]) -> bool {
    arrBytes.len() >= 4
        && u32::from_be_bytes([arrBytes[0], arrBytes[1], arrBytes[2], arrBytes[3]])
            == c_u32BlockSectionHeader
}

/// Read every packet from a pcapng file.
pub fn ReadPcapNg(arrBytes: &[u8]) -> Result<Vec<CapturedPacket>, PcapError> {
    let mut vecPackets = Vec::new();
    let mut vecInterfaces: Vec<Interface> = Vec::new();
    let mut bIsLittleEndian = true;
    let mut uOffset = 0usize;

    while uOffset + 12 <= arrBytes.len() {
        // A block's type is written in the section's byte order, but the section header's own
        // type is byte-order independent, so it can always be spotted first.
        let u32TypeBigEndian = u32::from_be_bytes([
            arrBytes[uOffset],
            arrBytes[uOffset + 1],
            arrBytes[uOffset + 2],
            arrBytes[uOffset + 3],
        ]);

        if u32TypeBigEndian == c_u32BlockSectionHeader {
            // A new section resets the interface table: interface ids are section-scoped.
            bIsLittleEndian = ReadSectionByteOrder(arrBytes, uOffset)?;
            vecInterfaces.clear();
        }

        let u32BlockType = ReadU32(arrBytes, uOffset, bIsLittleEndian);
        let u32BlockLength = ReadU32(arrBytes, uOffset + 4, bIsLittleEndian);

        // A block is at least a type, a length and a trailing length. Anything shorter would
        // not advance the cursor, so refusing here is what stops an infinite loop.
        if u32BlockLength < 12 {
            return Err(PcapError::Malformed {
                strReason: format!(
                    "a pcapng block at offset {uOffset} declares {u32BlockLength} bytes"
                ),
            });
        }
        let uBlockEnd = uOffset.saturating_add(u32BlockLength as usize);
        if uBlockEnd > arrBytes.len() {
            tracing::warn!(
                at = uOffset,
                declared = u32BlockLength,
                "the capture ends mid-block; returning what was read"
            );
            break;
        }

        let arrBody = &arrBytes[uOffset + 8..uBlockEnd - 4];

        match u32BlockType {
            c_u32BlockInterfaceDescription => {
                vecInterfaces.push(ReadInterface(arrBody, bIsLittleEndian));
            }
            c_u32BlockEnhancedPacket => {
                ReadEnhancedPacket(arrBody, bIsLittleEndian, &vecInterfaces, &mut vecPackets)?;
            }
            // Section headers and every other block type carry nothing this reader needs.
            _ => {}
        }

        uOffset = uBlockEnd;
    }

    // An Ethernet interface is required somewhere, or nothing in the file could ever be read.
    // Deciding it after the walk keeps a mixed-interface capture usable.
    if !vecInterfaces.is_empty()
        && !vecInterfaces
            .iter()
            .any(|interface| interface.m_u16LinkType == c_u16LinkTypeEthernet)
    {
        let u16LinkType = vecInterfaces[0].m_u16LinkType;
        return Err(PcapError::UnsupportedLinkType {
            u16LinkType,
            strName: LinkTypeName(u16LinkType),
        });
    }

    Ok(vecPackets)
}

/// Read a section header's byte-order magic.
fn ReadSectionByteOrder(arrBytes: &[u8], uOffset: usize) -> Result<bool, PcapError> {
    if arrBytes.len() < uOffset + 12 {
        return Err(PcapError::Truncated {
            strWhere: "a pcapng section header block".to_string(),
        });
    }
    let u32MagicBigEndian = u32::from_be_bytes([
        arrBytes[uOffset + 8],
        arrBytes[uOffset + 9],
        arrBytes[uOffset + 10],
        arrBytes[uOffset + 11],
    ]);
    Ok(u32MagicBigEndian != c_u32ByteOrderMagicBigEndian)
}

/// Read an interface description: its link type and timestamp resolution.
fn ReadInterface(arrBody: &[u8], bIsLittleEndian: bool) -> Interface {
    let u16LinkType = if arrBody.len() >= 2 {
        ReadU16(arrBody, 0, bIsLittleEndian)
    } else {
        c_u16LinkTypeEthernet
    };

    let byResolution = ReadTimestampResolution(arrBody, bIsLittleEndian);
    // The high bit selects base two; otherwise the value is a power of ten.
    let f64TicksPerSecond = if byResolution & 0x80 != 0 {
        2f64.powi((byResolution & 0x7F) as i32)
    } else {
        10f64.powi(byResolution as i32)
    };

    Interface {
        m_u16LinkType: u16LinkType,
        m_f64TicksPerSecond: f64TicksPerSecond,
    }
}

/// Find the `if_tsresol` option, or the default the format specifies.
fn ReadTimestampResolution(arrBody: &[u8], bIsLittleEndian: bool) -> u8 {
    // Options follow the fixed 8-byte interface body: link type, reserved, snap length.
    let mut uOffset = 8;
    while uOffset + 4 <= arrBody.len() {
        let u16Code = ReadU16(arrBody, uOffset, bIsLittleEndian);
        let u16Length = ReadU16(arrBody, uOffset + 2, bIsLittleEndian) as usize;
        if u16Code == 0 {
            break;
        }

        if u16Code == c_u16OptionTimestampResolution
            && u16Length >= 1
            && uOffset + 4 < arrBody.len()
        {
            return arrBody[uOffset + 4];
        }
        // Option values are padded to a multiple of four.
        uOffset += 4 + u16Length.div_ceil(4) * 4;
    }
    c_byDefaultTimestampResolution
}

/// Read one enhanced packet block into a captured packet.
fn ReadEnhancedPacket(
    arrBody: &[u8],
    bIsLittleEndian: bool,
    vecInterfaces: &[Interface],
    vecPackets: &mut Vec<CapturedPacket>,
) -> Result<(), PcapError> {
    if arrBody.len() < 20 {
        return Ok(());
    }

    let u32InterfaceId = ReadU32(arrBody, 0, bIsLittleEndian);
    let u32TimestampHigh = ReadU32(arrBody, 4, bIsLittleEndian);
    let u32TimestampLow = ReadU32(arrBody, 8, bIsLittleEndian);
    let u32CapturedLength = ReadU32(arrBody, 12, bIsLittleEndian) as usize;

    if arrBody.len() < 20 + u32CapturedLength {
        return Ok(());
    }

    let optInterface = vecInterfaces.get(u32InterfaceId as usize);
    // A packet on a non-Ethernet interface in an otherwise Ethernet capture is skipped rather
    // than mis-parsed; the whole-file check above catches a capture with no Ethernet at all.
    let f64TicksPerSecond = match optInterface {
        Some(interface) if interface.m_u16LinkType == c_u16LinkTypeEthernet => {
            interface.m_f64TicksPerSecond
        }
        Some(_) => return Ok(()),
        None => 1_000_000.0,
    };

    let u64Ticks = ((u32TimestampHigh as u64) << 32) | (u32TimestampLow as u64);
    let f64TimestampSec = u64Ticks as f64 / f64TicksPerSecond;

    if let Some(packet) = ReadEthernetFrame(&arrBody[20..20 + u32CapturedLength], f64TimestampSec)?
    {
        vecPackets.push(packet);
    }
    Ok(())
}

fn ReadU16(arrBytes: &[u8], uOffset: usize, bIsLittleEndian: bool) -> u16 {
    let arrField = [arrBytes[uOffset], arrBytes[uOffset + 1]];
    if bIsLittleEndian {
        u16::from_le_bytes(arrField)
    } else {
        u16::from_be_bytes(arrField)
    }
}

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
