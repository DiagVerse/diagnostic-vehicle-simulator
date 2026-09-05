//! ISO-TP (ISO 15765-2) reassembly — OSI L4 transport building block.
//!
//! Offline reassembly of a stream of CAN frames (already filtered to a single CAN ID) into
//! complete UDS PDUs. We are an *observer* of a recorded exchange, not a bus participant, so
//! flow-control frames are recognised and skipped rather than generated. Segmentation (the
//! transmit direction) will be added when the live CAN runtime needs it.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod params;
pub mod rx;
pub mod tx;

use can::CanFrame;

/// A fully reassembled ISO-TP message (one UDS PDU).
#[derive(Debug, Clone, PartialEq)]
pub struct IsoTpMessage {
    /// Timestamp of the frame that started the message (SF or FF).
    pub m_f64TimestampSec: f64,
    /// The complete UDS PDU bytes.
    pub m_vecData: Vec<u8>,
}

// ISO-TP Protocol Control Information (PCI) type is the high nibble of the first data byte.
const c_byPciSingleFrame: u8 = 0x0;
const c_byPciFirstFrame: u8 = 0x1;
const c_byPciConsecutiveFrame: u8 = 0x2;
const c_byPciFlowControl: u8 = 0x3;

/// In-progress multi-frame reassembly state.
struct Pending {
    m_vecBuffer: Vec<u8>,
    m_uRemaining: usize,
    m_f64TimestampSec: f64,
}

/// Reassemble one CAN-ID stream (time-ordered frames) into ISO-TP messages.
///
/// Malformed or unexpected frames are handled defensively: a new SF/FF abandons any
/// in-progress reassembly, and stray CF/FC frames without context are ignored.
pub fn ReassembleStream(vecFrames: &[CanFrame]) -> Vec<IsoTpMessage> {
    let mut vecMessages = Vec::new();
    let mut optPending: Option<Pending> = None;

    for frame in vecFrames {
        if frame.m_vecData.is_empty() {
            continue;
        }

        let byPciType = frame.m_vecData[0] >> 4;

        // While a multi-frame message is in progress, consecutive frames extend it and flow-
        // control frames are ignored without disturbing it. Anything else (an SF/FF) abandons
        // the incomplete message and is reprocessed as the start of a new one.
        if let Some(mut pending) = optPending.take() {
            if byPciType == c_byPciConsecutiveFrame {
                AppendConsecutive(&mut pending, &frame.m_vecData);
                if pending.m_uRemaining == 0 {
                    vecMessages.push(IsoTpMessage {
                        m_f64TimestampSec: pending.m_f64TimestampSec,
                        m_vecData: pending.m_vecBuffer,
                    });
                } else {
                    optPending = Some(pending);
                }
                continue;
            }
            if byPciType == c_byPciFlowControl {
                optPending = Some(pending);
                continue;
            }
            // SF/FF: leave `optPending` cleared (message abandoned) and fall through.
        }

        match byPciType {
            c_byPciSingleFrame => {
                if let Some(vecData) = DecodeSingleFrame(&frame.m_vecData) {
                    vecMessages.push(IsoTpMessage {
                        m_f64TimestampSec: frame.m_f64TimestampSec,
                        m_vecData: vecData,
                    });
                }
            }
            c_byPciFirstFrame => {
                optPending = StartFirstFrame(frame);
            }
            // A flow-control or stray consecutive frame outside a message: ignore.
            c_byPciFlowControl | c_byPciConsecutiveFrame => {}
            _ => {}
        }
    }

    vecMessages
}

/// Decode a single-frame PDU. Handles the CAN-FD escape form (length byte in data[1] when the
/// low nibble is zero).
fn DecodeSingleFrame(vecData: &[u8]) -> Option<Vec<u8>> {
    let uLowNibble = (vecData[0] & 0x0F) as usize;

    let (uLength, uStart) = if uLowNibble == 0 {
        // FD escape: real length is in the next byte, payload starts at index 2.
        if vecData.len() < 2 {
            return None;
        }
        (vecData[1] as usize, 2)
    } else {
        (uLowNibble, 1)
    };

    if uLength == 0 || uStart + uLength > vecData.len() {
        return None;
    }
    Some(vecData[uStart..uStart + uLength].to_vec())
}

/// Begin a multi-frame message from a first frame. Classic FF carries a 12-bit length in the
/// low nibble of byte 0 and byte 1, with 6 payload bytes following.
fn StartFirstFrame(frame: &CanFrame) -> Option<Pending> {
    let vecData = &frame.m_vecData;
    if vecData.len() < 2 {
        return None;
    }

    let uTotalLength = (((vecData[0] & 0x0F) as usize) << 8) | (vecData[1] as usize);
    if uTotalLength == 0 {
        return None;
    }

    let vecFirst: Vec<u8> = vecData[2..].to_vec();
    let uTaken = vecFirst.len().min(uTotalLength);

    // A well-formed first frame always leaves bytes for consecutive frames; if the payload
    // somehow already satisfies the length we simply record zero remaining.
    Some(Pending {
        m_vecBuffer: vecFirst[..uTaken].to_vec(),
        m_uRemaining: uTotalLength - uTaken,
        m_f64TimestampSec: frame.m_f64TimestampSec,
    })
}

/// Append a consecutive frame's payload to the pending buffer, up to the remaining length.
fn AppendConsecutive(pending: &mut Pending, vecData: &[u8]) {
    let vecPayload = &vecData[1..];
    let uTake = vecPayload.len().min(pending.m_uRemaining);
    pending.m_vecBuffer.extend_from_slice(&vecPayload[..uTake]);
    pending.m_uRemaining -= uTake;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: u32, data: Vec<u8>) -> CanFrame {
        CanFrame::NewClassic(0.0, id, data)
    }

    #[test]
    fn single_frame_is_decoded() {
        let frames = vec![frame(
            0x7E0,
            vec![0x02, 0x10, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
        )];
        let msgs = ReassembleStream(&frames);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].m_vecData, vec![0x10, 0x03]);
    }

    #[test]
    fn multi_frame_is_reassembled() {
        // A 20-byte response: 62 F1 90 followed by 17 VIN bytes.
        // FF carries length 0x014 = 20 and the first 6 bytes; two CFs carry 7 each.
        let frames = vec![
            frame(0x7E8, vec![0x10, 0x14, 0x62, 0xF1, 0x90, b'V', b'I', b'N']),
            frame(0x7E8, vec![0x21, b'0', b'1', b'2', b'3', b'4', b'5', b'6']),
            frame(0x7E8, vec![0x22, b'7', b'8', b'9', b'A', b'B', b'C', b'D']),
        ];
        let msgs = ReassembleStream(&frames);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].m_vecData.len(), 20);
        assert_eq!(&msgs[0].m_vecData[0..3], &[0x62, 0xF1, 0x90]);
        assert_eq!(&msgs[0].m_vecData[3..], b"VIN0123456789ABCD"[..17].as_ref());
    }

    #[test]
    fn flow_control_frames_are_ignored() {
        let frames = vec![
            frame(0x7E8, vec![0x10, 0x0A, 0x62, 0xF1, 0x90, 0x01, 0x02, 0x03]),
            frame(0x7E0, vec![0x30, 0x00, 0x00]), // FC from tester (different ID normally)
            frame(0x7E8, vec![0x21, 0x04, 0x05, 0x06, 0x07, 0x00, 0x00, 0x00]),
        ];
        // Only the 0x7E8 stream should be passed here in practice; include FC to prove skip.
        let msgs = ReassembleStream(&frames);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].m_vecData.len(), 10);
    }
}
