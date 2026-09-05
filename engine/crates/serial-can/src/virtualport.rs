//! A transport for **virtual** ports: a pseudo-terminal on Unix, a `com0com`-style pair on
//! Windows.
//!
//! These exist so a tester tool on the same machine can be wired to the engine with no
//! hardware between them. They cannot be opened the way a real port is: a pseudo-terminal has
//! no line speed, no parity and no flow-control lines, so the ioctls a serial library applies
//! come back as "not a typewriter" and the open fails outright.
//!
//! Opening the device as a plain file avoids all of that. Nothing is lost — there is no UART
//! to configure — and the bytes cross exactly as they would over a real link.

#![allow(non_snake_case, non_upper_case_globals)]

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};

use crate::{SerialError, SerialTransport};

/// A virtual port opened as a file.
pub struct VirtualPortTransport {
    m_strName: String,
    m_file: File,
}

impl VirtualPortTransport {
    /// Open a virtual port for reading and writing.
    pub fn Open(strPortName: &str) -> Result<Self, SerialError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(strPortName)
            .map_err(|error| SerialError::Open {
                strPortName: strPortName.to_string(),
                strReason: error.to_string(),
            })?;

        MakeNonBlocking(&file, strPortName)?;

        tracing::info!(
            port = %strPortName,
            "virtual port opened; no line settings apply to one, so none were requested"
        );
        Ok(VirtualPortTransport {
            m_strName: strPortName.to_string(),
            m_file: file,
        })
    }
}

/// Put the handle in non-blocking mode so a read of an idle port returns instead of parking
/// the bridge forever.
#[cfg(unix)]
fn MakeNonBlocking(file: &File, strPortName: &str) -> Result<(), SerialError> {
    use std::os::fd::AsRawFd;

    let iFileDescriptor = file.as_raw_fd();
    // SAFETY: `iFileDescriptor` comes from a File this function holds open for the duration of
    // the call, and both fcntl commands used here only read and set that descriptor's flags.
    let iResult = unsafe {
        let iFlags = libc::fcntl(iFileDescriptor, libc::F_GETFL);
        libc::fcntl(iFileDescriptor, libc::F_SETFL, iFlags | libc::O_NONBLOCK)
    };

    if iResult < 0 {
        return Err(SerialError::Open {
            strPortName: strPortName.to_string(),
            strReason: "could not put the port into non-blocking mode".to_string(),
        });
    }
    Ok(())
}

/// Windows virtual ports behave enough like files that no extra step is needed here; a read
/// that finds nothing is reported by the platform rather than by blocking indefinitely.
#[cfg(not(unix))]
fn MakeNonBlocking(_file: &File, _strPortName: &str) -> Result<(), SerialError> {
    Ok(())
}

impl SerialTransport for VirtualPortTransport {
    fn Name(&self) -> &str {
        &self.m_strName
    }

    fn Write(&mut self, vecBytes: &[u8]) -> Result<(), SerialError> {
        self.m_file
            .write_all(vecBytes)
            .map_err(|error| SerialError::Io {
                strPortName: self.m_strName.clone(),
                strReason: error.to_string(),
            })
    }

    fn Read(&mut self, vecBuffer: &mut [u8]) -> Result<usize, SerialError> {
        match self.m_file.read(vecBuffer) {
            Ok(uCount) => Ok(uCount),
            // Nothing has arrived. That is an idle link, not a failure.
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(0)
            }
            Err(error) => Err(SerialError::Io {
                strPortName: self.m_strName.clone(),
                strReason: error.to_string(),
            }),
        }
    }
}
