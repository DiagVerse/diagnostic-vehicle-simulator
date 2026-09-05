//! SLCAN / LAWICEL — the ASCII line protocol USB-CAN adapters speak.
//!
//! Not a standard. It comes from a Lawicel CANUSB datasheet and was re-implemented
//! independently by CANable, USBtin, CANtact and others, so the command set below is what
//! those adapters have in common rather than anything normative. Where a command is
//! vendor-specific it is marked as such and this crate does not depend on it.
//!
//! Pure codec: bytes in, [`CanFrame`]s out, and back. No I/O and no async, so every rule here
//! is testable without a serial port.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod decoder;

use can::CanFrame;

/// Every SLCAN line ends with a carriage return. Never a line feed.
pub const c_byTerminator: u8 = b'\r';
/// What an adapter sends to reject a command.
pub const c_byBell: u8 = 0x07;

/// Longest line worth accumulating before giving up on a device that never terminates one.
/// A 64-byte CAN-FD frame is about 140 characters, so this leaves generous room.
pub const c_uMaxLineLength: usize = 200;

/// The bitrates an adapter's `S` command selects. The mapping is identical across every
/// adapter family, which is why it can be hard-coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlcanBitrate {
    Kbit10,
    Kbit20,
    Kbit50,
    Kbit100,
    Kbit125,
    Kbit250,
    Kbit500,
    Kbit800,
    Mbit1,
}

impl SlcanBitrate {
    /// The `S<n>` digit this bitrate is selected with.
    pub fn ToCommandDigit(self) -> char {
        match self {
            SlcanBitrate::Kbit10 => '0',
            SlcanBitrate::Kbit20 => '1',
            SlcanBitrate::Kbit50 => '2',
            SlcanBitrate::Kbit100 => '3',
            SlcanBitrate::Kbit125 => '4',
            SlcanBitrate::Kbit250 => '5',
            SlcanBitrate::Kbit500 => '6',
            SlcanBitrate::Kbit800 => '7',
            SlcanBitrate::Mbit1 => '8',
        }
    }

    /// Bits per second, for logs and for the UI.
    pub fn ToBitsPerSecond(self) -> u32 {
        match self {
            SlcanBitrate::Kbit10 => 10_000,
            SlcanBitrate::Kbit20 => 20_000,
            SlcanBitrate::Kbit50 => 50_000,
            SlcanBitrate::Kbit100 => 100_000,
            SlcanBitrate::Kbit125 => 125_000,
            SlcanBitrate::Kbit250 => 250_000,
            SlcanBitrate::Kbit500 => 500_000,
            SlcanBitrate::Kbit800 => 800_000,
            SlcanBitrate::Mbit1 => 1_000_000,
        }
    }

    /// Pick the bitrate for a bits-per-second value, or `None` if no adapter setting matches.
    pub fn FromBitsPerSecond(u32BitsPerSecond: u32) -> Option<SlcanBitrate> {
        let arrAll = [
            SlcanBitrate::Kbit10,
            SlcanBitrate::Kbit20,
            SlcanBitrate::Kbit50,
            SlcanBitrate::Kbit100,
            SlcanBitrate::Kbit125,
            SlcanBitrate::Kbit250,
            SlcanBitrate::Kbit500,
            SlcanBitrate::Kbit800,
            SlcanBitrate::Mbit1,
        ];
        arrAll
            .into_iter()
            .find(|rate| rate.ToBitsPerSecond() == u32BitsPerSecond)
    }
}

/// Why a line could not be decoded.
///
/// Every one of these is something a browning-out dongle really emits, so none of them may
/// panic and all of them carry the offending line for the log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlcanDecodeError {
    /// The line was shorter than its own header claims.
    #[error("SLCAN line '{strLine}' is too short for a {strKind} frame")]
    TooShort { strLine: String, strKind: String },

    /// A character that should have been hex was not.
    #[error("SLCAN line '{strLine}' contains non-hex characters where a number was expected")]
    NotHex { strLine: String },

    /// The data-length code is outside what CAN allows.
    #[error("SLCAN line '{strLine}' declares length {uLength}, but a classic CAN frame holds at most 8 bytes")]
    BadLength { strLine: String, uLength: usize },

    /// The declared length and the bytes actually present disagree.
    #[error("SLCAN line '{strLine}' declares {uDeclared} data bytes but carries {uPresent}")]
    LengthMismatch {
        strLine: String,
        uDeclared: usize,
        uPresent: usize,
    },
}

/// Encode a CAN frame as the line an adapter transmits it with.
///
/// Always uppercase: adapters accept either, but a consistent capture is easier to read and to
/// diff against a reference log.
pub fn EncodeFrame(frame: &CanFrame) -> String {
    let mut strLine = String::with_capacity(c_uMaxLineLength);

    if frame.m_bIsExtended {
        strLine.push('T');
        strLine.push_str(&format!("{:08X}", frame.m_u32CanId));
    } else {
        strLine.push('t');
        strLine.push_str(&format!("{:03X}", frame.m_u32CanId));
    }

    strLine.push_str(&format!("{:X}", frame.m_vecData.len()));
    for byByte in &frame.m_vecData {
        strLine.push_str(&format!("{byByte:02X}"));
    }
    strLine.push(c_byTerminator as char);
    strLine
}

/// Commands that open a channel at a bitrate, in the order an adapter requires.
///
/// The close comes first on purpose: an adapter left open by a crashed process rejects the
/// bitrate command, and the resulting rejection is indistinguishable from "wrong bitrate"
/// without it.
pub fn OpenCommands(bitrate: SlcanBitrate) -> Vec<String> {
    vec![
        "C\r".to_string(),
        format!("S{}\r", bitrate.ToCommandDigit()),
        "O\r".to_string(),
    ]
}

/// The command that closes the channel.
pub fn CloseCommand() -> String {
    "C\r".to_string()
}

/// The command that reads and clears the adapter's status flags.
pub fn StatusCommand() -> String {
    "F\r".to_string()
}

/// Decode the status byte an adapter answers `F` with. Bit meanings are inherited from the
/// SJA1000 controller the original adapters used.
pub fn DescribeStatusFlags(byFlags: u8) -> Vec<&'static str> {
    let arrBits: [(u8, &'static str); 7] = [
        (0x01, "receive FIFO full"),
        (0x02, "transmit FIFO full"),
        (0x04, "error warning"),
        (0x08, "data overrun"),
        (0x20, "error passive"),
        (0x40, "arbitration lost"),
        (0x80, "bus off"),
    ];

    arrBits
        .into_iter()
        .filter(|(byMask, _)| (byFlags & byMask) != 0)
        .map(|(_, strName)| strName)
        .collect()
}
