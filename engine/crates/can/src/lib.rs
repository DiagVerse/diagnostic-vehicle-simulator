//! CAN / CAN-FD frame types — the OSI L2 data-link building block.
//!
//! This is a small library crate (see ADR 0002): frame types shared by the ISO-TP layer, the
//! CAN-log populator, and (later) the live CAN runtime. It carries no I/O.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod busload;
pub mod confinement;

/// A single CAN or CAN-FD frame as observed on a bus or read from a log.
#[derive(Debug, Clone, PartialEq)]
pub struct CanFrame {
    /// Capture time in seconds (relative to the log start or an absolute clock).
    pub m_f64TimestampSec: f64,
    /// CAN identifier (11-bit or 29-bit; extended-ness is tracked separately).
    pub m_u32CanId: u32,
    /// True for a 29-bit extended identifier.
    pub m_bIsExtended: bool,
    /// True for a CAN-FD frame (payload may exceed 8 bytes).
    pub m_bIsFd: bool,
    /// Payload bytes (0..=8 for classic CAN, up to 64 for CAN-FD). Always empty for a remote
    /// frame, which asks for data rather than carrying it.
    pub m_vecData: Vec<u8>,
    /// True for a remote transmission request — a frame that asks another node to send the
    /// data for this identifier.
    ///
    /// Tracked rather than flattened into "a frame with no payload". The two are different on
    /// the wire (the RTR bit) and mean opposite things: one supplies data, the other asks for
    /// it. A decoder that dropped the distinction reported every remote request as an empty
    /// data frame, which is a claim the bus never made.
    pub m_bIsRemote: bool,
    /// The data length a remote frame asks for. Zero for every data frame, which carries its
    /// length in the payload instead.
    pub m_uRemoteLength: usize,
    /// Whether this frame travelled from the tester to an ECU, when the source said so.
    /// `None` when the format carries no direction marker and it has to be inferred.
    pub m_optBIsRequest: Option<bool>,
}

impl CanFrame {
    /// Construct a classic (non-FD, 11-bit) CAN frame.
    pub fn NewClassic(f64TimestampSec: f64, u32CanId: u32, vecData: Vec<u8>) -> Self {
        CanFrame {
            m_f64TimestampSec: f64TimestampSec,
            m_u32CanId: u32CanId,
            m_bIsExtended: u32CanId > 0x7FF,
            m_bIsFd: false,
            m_vecData: vecData,
            m_bIsRemote: false,
            m_uRemoteLength: 0,
            m_optBIsRequest: None,
        }
    }

    /// Construct a remote transmission request, which carries a length but no data.
    pub fn NewRemote(f64TimestampSec: f64, u32CanId: u32, uRequestedLength: usize) -> Self {
        CanFrame {
            m_uRemoteLength: uRequestedLength.min(c_uMaxClassicPayload),
            m_bIsRemote: true,
            ..CanFrame::NewClassic(f64TimestampSec, u32CanId, Vec::new())
        }
    }

    /// Construct a classic frame whose source recorded which way it travelled.
    pub fn NewDirected(
        f64TimestampSec: f64,
        u32CanId: u32,
        vecData: Vec<u8>,
        bIsRequest: bool,
    ) -> Self {
        CanFrame {
            m_optBIsRequest: Some(bIsRequest),
            ..CanFrame::NewClassic(f64TimestampSec, u32CanId, vecData)
        }
    }

    /// Payload length in bytes (the CAN DLC once decoded).
    pub fn DataLength(&self) -> usize {
        self.m_vecData.len()
    }

    /// The data-length code this frame carries, which for a remote frame is the length being
    /// asked for rather than any length present.
    pub fn DataLengthCode(&self) -> usize {
        if self.m_bIsRemote {
            self.m_uRemoteLength
        } else {
            self.m_vecData.len()
        }
    }

    /// Bits this frame occupies on the wire, before bit stuffing.
    ///
    /// Exact for the fields the standard fixes (ISO 11898-1 clause 10.4): start of frame,
    /// arbitration, control, data, CRC and its delimiter, the acknowledge slot and delimiter,
    /// end of frame, and the interframe space that must follow before the next frame may start.
    ///
    /// Stuffing is *not* included, and deliberately: a stuff bit is inserted after five
    /// identical consecutive bits, so the real length depends on the payload's bit pattern and
    /// on a CRC this type does not carry. Counting it would mean inventing a number. Bus load
    /// computed from this is therefore a floor — see [`MaxStuffBits`] for the other end of the
    /// range.
    pub fn BitsOnWire(&self) -> usize {
        let uArbitrationAndControl = if self.m_bIsExtended {
            c_uExtendedFrameOverheadBits
        } else {
            c_uStandardFrameOverheadBits
        };

        let uDataBits = if self.m_bIsRemote {
            0
        } else {
            self.m_vecData.len() * 8
        };

        uArbitrationAndControl + uDataBits
    }

    /// The most stuff bits this frame could attract.
    ///
    /// Stuffing covers everything from the start of frame through the CRC — end of frame, the
    /// acknowledge field and the interframe space are fixed-form and never stuffed. One bit is
    /// added at worst per four stuffable bits.
    pub fn MaxStuffBits(&self) -> usize {
        let uStuffable = self.BitsOnWire().saturating_sub(c_uUnstuffedTrailerBits);
        uStuffable / 4
    }
}

/// Largest payload a classic CAN frame carries.
pub const c_uMaxClassicPayload: usize = 8;

/// Fixed bits in a standard (11-bit) frame with no payload: start of frame, the 11-bit
/// identifier, RTR, IDE, r0, the four-bit DLC, a 15-bit CRC and delimiter, the acknowledge slot
/// and delimiter, seven end-of-frame bits and three of interframe space.
const c_uStandardFrameOverheadBits: usize = 47;

/// The same for an extended (29-bit) frame, which adds SRR, IDE, the 18-bit identifier
/// extension and a second reserved bit.
const c_uExtendedFrameOverheadBits: usize = 67;

/// Bits at the end of every frame that bit stuffing never touches: CRC delimiter, the
/// acknowledge slot and delimiter, end of frame, and the interframe space.
const c_uUnstuffedTrailerBits: usize = 13;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_frame_infers_extended_from_id() {
        let frame = CanFrame::NewClassic(0.0, 0x7E0, vec![0x02, 0x10, 0x03]);
        assert!(!frame.m_bIsExtended);
        assert_eq!(frame.DataLength(), 3);

        let extended = CanFrame::NewClassic(0.0, 0x18DA10F1, vec![]);
        assert!(extended.m_bIsExtended);
    }
}
