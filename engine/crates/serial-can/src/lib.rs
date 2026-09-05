//! Serial transports — the wire under a CAN bridge.
//!
//! This crate knows nothing about CAN or SLCAN. It moves bytes to and from a serial port, and
//! offers an in-memory loopback so everything above it can be tested without hardware. Keeping
//! the split here means the protocol layers stay pure and the only untestable part is a
//! handful of lines that talk to the operating system.
//!
//! The real port sits behind the `serial` feature so a host without the serial stack — CI, a
//! container — can still build every layer above.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod loopback;
#[cfg(feature = "serial")]
pub mod port;

use std::time::Duration;

/// Why a serial operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SerialError {
    /// The port could not be opened: wrong name, already in use, no permission.
    #[error("could not open serial port '{strPortName}': {strReason}")]
    Open {
        /// The port that was asked for.
        strPortName: String,
        /// What the operating system said.
        strReason: String,
    },

    /// A read or write failed once the port was open.
    #[error("serial I/O failed on '{strPortName}': {strReason}")]
    Io {
        /// The port in use.
        strPortName: String,
        /// What the operating system said.
        strReason: String,
    },

    /// The build has no serial support compiled in.
    #[error("this build has no serial support; rebuild with the 'serial' feature to open a port")]
    NotSupported,
}

/// A serial port's identity, as offered to a user choosing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortInfo {
    /// What to pass back to open it, e.g. `/dev/tty.usbmodem1101` or `COM3`.
    pub m_strName: String,
    /// A human-readable hint about what is plugged in, when the OS offers one.
    pub m_strDescription: String,
}

/// A bidirectional byte stream.
///
/// Deliberately blocking and byte-oriented: SLCAN is a line protocol over a serial port, and
/// framing belongs to the layer above. Implementations must return promptly with zero bytes
/// rather than blocking forever when nothing has arrived, so a bridge can poll for shutdown.
pub trait SerialTransport: Send {
    /// The port's name, for logs and errors.
    fn Name(&self) -> &str;

    /// Send bytes. Returns once they are handed to the operating system.
    fn Write(&mut self, vecBytes: &[u8]) -> Result<(), SerialError>;

    /// Read whatever has arrived, up to the buffer's size. Returns `Ok(0)` when nothing has,
    /// which is not an error — it is the normal state of an idle bus.
    fn Read(&mut self, vecBuffer: &mut [u8]) -> Result<usize, SerialError>;
}

/// How long a read waits before reporting that nothing arrived. Short enough that a bridge
/// notices a stop request promptly, long enough not to spin the CPU.
pub const c_readTimeout: Duration = Duration::from_millis(20);

/// List the serial ports this machine offers.
///
/// Returns an empty list rather than an error when the build has no serial support: a caller
/// asking "what can I connect to?" is better served by "nothing" than by a failure.
pub fn ListPorts() -> Vec<SerialPortInfo> {
    #[cfg(feature = "serial")]
    {
        port::ListPorts()
    }
    #[cfg(not(feature = "serial"))]
    {
        tracing::debug!("serial support is not compiled in; no ports to list");
        Vec::new()
    }
}

/// Open a serial port by name at the given baud rate.
pub fn OpenPort(
    strPortName: &str,
    u32BaudRate: u32,
) -> Result<Box<dyn SerialTransport>, SerialError> {
    #[cfg(feature = "serial")]
    {
        let transport = port::SerialPortTransport::Open(strPortName, u32BaudRate)?;
        Ok(Box::new(transport))
    }
    #[cfg(not(feature = "serial"))]
    {
        let _ = (strPortName, u32BaudRate);
        Err(SerialError::NotSupported)
    }
}
