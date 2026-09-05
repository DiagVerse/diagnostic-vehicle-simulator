//! The real serial port, behind the `serial` feature.
//!
//! This is the only file in the crate that talks to the operating system, which is why every
//! layer above it can be tested with the loopback transport instead.

#![allow(non_snake_case, non_upper_case_globals)]

use std::io::{Read as _, Write as _};

use crate::{c_readTimeout, SerialError, SerialPortInfo, SerialTransport};

/// A serial port opened for reading and writing.
pub struct SerialPortTransport {
    m_strName: String,
    m_boxPort: Box<dyn serialport::SerialPort>,
}

impl SerialPortTransport {
    /// Open a port by name at a baud rate.
    ///
    /// The read timeout is short on purpose: a bridge polls this in a loop and must notice a
    /// stop request promptly, so a read that finds nothing has to return rather than block.
    pub fn Open(strPortName: &str, u32BaudRate: u32) -> Result<Self, SerialError> {
        let boxPort = serialport::new(strPortName, u32BaudRate)
            .timeout(c_readTimeout)
            .open()
            .map_err(|error| SerialError::Open {
                strPortName: strPortName.to_string(),
                strReason: error.to_string(),
            })?;

        tracing::info!(port = %strPortName, baud = u32BaudRate, "serial port opened");
        Ok(SerialPortTransport {
            m_strName: strPortName.to_string(),
            m_boxPort: boxPort,
        })
    }
}

impl SerialTransport for SerialPortTransport {
    fn Name(&self) -> &str {
        &self.m_strName
    }

    fn Write(&mut self, vecBytes: &[u8]) -> Result<(), SerialError> {
        self.m_boxPort
            .write_all(vecBytes)
            .map_err(|error| SerialError::Io {
                strPortName: self.m_strName.clone(),
                strReason: error.to_string(),
            })
    }

    fn Read(&mut self, vecBuffer: &mut [u8]) -> Result<usize, SerialError> {
        match self.m_boxPort.read(vecBuffer) {
            Ok(uCount) => Ok(uCount),
            // A timeout means the bus was quiet, which is the normal state of an idle link and
            // not something to report as a failure.
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Ok(0),
            Err(error) => Err(SerialError::Io {
                strPortName: self.m_strName.clone(),
                strReason: error.to_string(),
            }),
        }
    }
}

/// Enumerate the serial ports the operating system reports.
pub fn ListPorts() -> Vec<SerialPortInfo> {
    let vecPorts = match serialport::available_ports() {
        Ok(vecPorts) => vecPorts,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate serial ports");
            return Vec::new();
        }
    };

    vecPorts
        .into_iter()
        .map(|port| SerialPortInfo {
            m_strDescription: DescribePort(&port),
            m_strName: port.port_name,
        })
        .collect()
}

/// A human-readable hint about what is plugged into a port.
fn DescribePort(port: &serialport::SerialPortInfo) -> String {
    match &port.port_type {
        serialport::SerialPortType::UsbPort(usb) => {
            let strProduct = usb
                .product
                .clone()
                .unwrap_or_else(|| "USB serial".to_string());
            let strManufacturer = usb.manufacturer.clone().unwrap_or_default();
            if strManufacturer.is_empty() {
                strProduct
            } else {
                format!("{strManufacturer} {strProduct}")
            }
        }
        serialport::SerialPortType::BluetoothPort => "Bluetooth".to_string(),
        serialport::SerialPortType::PciPort => "PCI".to_string(),
        serialport::SerialPortType::Unknown => "serial port".to_string(),
    }
}
