//! A CAN bus the engine can send on and receive from.
//!
//! One trait, two implementations: a real adapter reached over a serial port, and an
//! in-memory one for tests. Everything above this line is identical either way, which is what
//! lets the whole bridge be tested without hardware.

#![allow(non_snake_case, non_upper_case_globals)]

use can::CanFrame;
use serial_can::{SerialError, SerialTransport};
use slcan::decoder::{SlcanDecoder, SlcanEvent};
use slcan::SlcanBitrate;

/// Somewhere CAN frames go and come from.
pub trait CanBusPort: Send {
    /// Put a frame on the bus.
    fn SendFrame(&mut self, frame: &CanFrame) -> Result<(), SerialError>;

    /// Take whatever has arrived since the last call. Returns an empty vector when the bus has
    /// been quiet, which is its normal state rather than a failure.
    fn ReceiveFrames(&mut self, f64TimestampSec: f64) -> Result<Vec<CanFrame>, SerialError>;

    /// A name for logs.
    fn Describe(&self) -> String;
}

/// A CAN bus reached through a USB adapter speaking SLCAN over a serial port.
pub struct SlcanBus {
    m_boxTransport: Box<dyn SerialTransport>,
    m_decoder: SlcanDecoder,
    /// Counts adapter rejections, so a mis-ordered command sequence shows up as a number in
    /// the log rather than as mysteriously absent traffic.
    m_uRejectionCount: usize,
}

impl SlcanBus {
    /// Open the adapter's CAN channel at a bitrate.
    ///
    /// The command order matters and is not negotiable: close, set bitrate, open. An adapter
    /// left open by a crashed process rejects the bitrate command, and that rejection is
    /// indistinguishable from "wrong bitrate" without the defensive close.
    pub fn Open(
        mut boxTransport: Box<dyn SerialTransport>,
        bitrate: SlcanBitrate,
    ) -> Result<Self, SerialError> {
        for strCommand in slcan::OpenCommands(bitrate) {
            boxTransport.Write(strCommand.as_bytes())?;
        }

        tracing::info!(
            port = %boxTransport.Name(),
            bitrate = bitrate.ToBitsPerSecond(),
            "CAN channel opened"
        );
        Ok(SlcanBus {
            m_boxTransport: boxTransport,
            m_decoder: SlcanDecoder::New(),
            m_uRejectionCount: 0,
        })
    }

    /// Close the adapter's CAN channel.
    pub fn Close(&mut self) -> Result<(), SerialError> {
        self.m_boxTransport
            .Write(slcan::CloseCommand().as_bytes())?;
        tracing::info!(port = %self.m_boxTransport.Name(), rejections = self.m_uRejectionCount, "CAN channel closed");
        Ok(())
    }
}

impl CanBusPort for SlcanBus {
    fn SendFrame(&mut self, frame: &CanFrame) -> Result<(), SerialError> {
        let strLine = slcan::EncodeFrame(frame);
        tracing::trace!(line = %strLine.trim_end(), "transmitting");
        self.m_boxTransport.Write(strLine.as_bytes())
    }

    fn ReceiveFrames(&mut self, f64TimestampSec: f64) -> Result<Vec<CanFrame>, SerialError> {
        let mut arrBuffer = [0u8; 1024];
        let uCount = self.m_boxTransport.Read(&mut arrBuffer)?;
        if uCount == 0 {
            return Ok(Vec::new());
        }

        let mut vecFrames = Vec::new();
        for event in self.m_decoder.Feed(&arrBuffer[..uCount], f64TimestampSec) {
            match event {
                SlcanEvent::Frame(frame) => vecFrames.push(frame),
                SlcanEvent::Nack => {
                    self.m_uRejectionCount += 1;
                    tracing::debug!(
                        rejections = self.m_uRejectionCount,
                        "the adapter rejected a command"
                    );
                }
                // Acknowledgements are consumed opportunistically: firmwares differ over
                // whether they send one at all, so waiting for them would stall on some
                // adapters and not on others.
                SlcanEvent::Ack => {}
                SlcanEvent::Malformed(_) => {}
            }
        }
        Ok(vecFrames)
    }

    fn Describe(&self) -> String {
        format!("SLCAN adapter on {}", self.m_boxTransport.Name())
    }
}
