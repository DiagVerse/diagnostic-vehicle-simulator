//! ISO-TP link parameters and the timings that govern a transfer.
//!
//! Values marked as ISO 15765-2 defaults were not verifiable against a copy of that standard
//! while this was written; they are the widely-implemented values and should be checked
//! against the standard before anyone relies on them for conformance testing.

#![allow(non_snake_case, non_upper_case_globals)]

use std::time::Duration;

/// The padding byte this engine uses unless told otherwise.
///
/// `0xAA` is `10101010`, which produces no bit stuffing — CAN inserts a stuff bit after five
/// identical consecutive bits, so `0x00` and `0xFF` padding lengthens the frame on the wire
/// while this does not. It also distinguishes engine-generated padding from the `0x55` a
/// tester commonly uses, which is worth something when reading a capture.
pub const c_byDefaultPaddingByte: u8 = 0xAA;

/// Payload bytes in a classic CAN frame.
pub const c_uClassicFrameLength: usize = 8;
/// Largest PDU a SingleFrame can carry with normal addressing.
pub const c_uMaxSingleFrameLength: usize = c_uClassicFrameLength - 1;
/// Payload bytes a FirstFrame carries after its two PCI bytes.
pub const c_uFirstFramePayload: usize = c_uClassicFrameLength - 2;
/// Payload bytes a ConsecutiveFrame carries after its one PCI byte.
pub const c_uConsecutiveFramePayload: usize = c_uClassicFrameLength - 1;
/// Largest PDU a classic FirstFrame can announce: its length field is 12 bits.
pub const c_uMaxClassicPduLength: usize = 4095;

/// Timeout waiting for a FlowControl after a FirstFrame or the last frame of a block.
pub const c_timeoutFlowControl: Duration = Duration::from_millis(1000);
/// Timeout waiting for the next ConsecutiveFrame of an inbound message.
pub const c_timeoutConsecutiveFrame: Duration = Duration::from_millis(1000);

/// How many consecutive "wait" flow-control frames to accept before giving up. ISO 15765-2
/// leaves this to the implementer.
pub const c_u8MaxWaitFrames: u8 = 10;

/// Separation time to use when a tester sends a reserved value.
///
/// The standard requires the **most conservative** interpretation, not the fastest — this is
/// the rule implementations most often get backwards, and getting it wrong floods a tester
/// that was trying to slow the server down.
pub const c_bySeparationTimeOnReserved: u8 = 0x7F;

/// What one end of a link says about how it wants to be spoken to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoTpParameters {
    /// How many consecutive frames may be sent before another flow control is required.
    /// Zero means "send them all".
    pub m_u8BlockSize: u8,
    /// The raw separation-time byte, kept exactly as it travels on the wire.
    ///
    /// Stored raw rather than decoded: round-tripping `0xF1` through a millisecond field would
    /// lose the 100 microseconds and re-emit `0x00`, silently changing what the ECU says about
    /// itself.
    pub m_bySeparationTimeMin: u8,
    /// Byte to pad short frames with, or `None` to send them short.
    pub m_optByPaddingByte: Option<u8>,
}

impl Default for IsoTpParameters {
    /// What this engine advertises when receiving: no block limit, no separation time.
    ///
    /// Not an arbitrary choice. Block size and separation time exist so a receiver can declare
    /// **its own** buffer and processing limits, and this receiver is a `Vec<u8>` on a host
    /// with gigabytes. Advertising a plausible-looking ECU value would be the engine asserting
    /// a constraint it does not have.
    fn default() -> Self {
        IsoTpParameters {
            m_u8BlockSize: 0,
            m_bySeparationTimeMin: 0,
            m_optByPaddingByte: Some(c_byDefaultPaddingByte),
        }
    }
}

/// Decode a separation-time byte into a duration.
///
/// `0x00`–`0x7F` are milliseconds; `0xF1`–`0xF9` are 100–900 microseconds; everything else is
/// reserved and becomes the most conservative value the standard allows.
pub fn DecodeSeparationTime(bySeparationTime: u8) -> Duration {
    if bySeparationTime <= 0x7F {
        return Duration::from_millis(bySeparationTime as u64);
    }

    if (0xF1..=0xF9).contains(&bySeparationTime) {
        let u64Microseconds = (bySeparationTime - 0xF0) as u64 * 100;
        return Duration::from_micros(u64Microseconds);
    }

    tracing::debug!(
        raw = format!("{bySeparationTime:02X}"),
        "reserved separation-time value; using the most conservative 127 ms as the standard requires"
    );
    Duration::from_millis(c_bySeparationTimeOnReserved as u64)
}

/// Pad a frame out to the classic 8-byte length, if padding is configured.
pub fn ApplyPadding(vecFrame: &mut Vec<u8>, optByPaddingByte: Option<u8>) {
    let byPaddingByte = match optByPaddingByte {
        Some(byPaddingByte) => byPaddingByte,
        None => return,
    };

    while vecFrame.len() < c_uClassicFrameLength {
        vecFrame.push(byPaddingByte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separation_times_decode_in_both_units() {
        assert_eq!(DecodeSeparationTime(0x00), Duration::ZERO);
        assert_eq!(DecodeSeparationTime(0x14), Duration::from_millis(20));
        assert_eq!(DecodeSeparationTime(0x7F), Duration::from_millis(127));
        assert_eq!(DecodeSeparationTime(0xF1), Duration::from_micros(100));
        assert_eq!(DecodeSeparationTime(0xF9), Duration::from_micros(900));
    }

    #[test]
    fn a_reserved_separation_time_becomes_the_slowest_not_the_fastest() {
        // The rule implementations most often invert. A tester sending a reserved value was
        // trying to slow the server down; answering with 0 ms would flood it.
        for byReserved in [0x80, 0x90, 0xF0, 0xFA, 0xFF] {
            assert_eq!(
                DecodeSeparationTime(byReserved),
                Duration::from_millis(127),
                "reserved value {byReserved:02X}"
            );
        }
    }

    #[test]
    fn padding_fills_to_eight_bytes_or_is_left_off() {
        let mut vecFrame = vec![0x02, 0x10, 0x03];
        ApplyPadding(&mut vecFrame, Some(0xAA));
        assert_eq!(
            vecFrame,
            vec![0x02, 0x10, 0x03, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]
        );

        let mut vecShort = vec![0x02, 0x10, 0x03];
        ApplyPadding(&mut vecShort, None);
        assert_eq!(vecShort.len(), 3);
    }
}
