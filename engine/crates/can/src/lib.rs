//! CAN / CAN-FD frame types — the OSI L2 data-link building block.
//!
//! This is a small library crate (see ADR 0002): frame types shared by the ISO-TP layer, the
//! CAN-log populator, and (later) the live CAN runtime. It carries no I/O.

#![allow(non_snake_case, non_upper_case_globals)]

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
    /// Payload bytes (0..=8 for classic CAN, up to 64 for CAN-FD).
    pub m_vecData: Vec<u8>,
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
        }
    }

    /// Payload length in bytes (the CAN DLC once decoded).
    pub fn DataLength(&self) -> usize {
        self.m_vecData.len()
    }
}

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
