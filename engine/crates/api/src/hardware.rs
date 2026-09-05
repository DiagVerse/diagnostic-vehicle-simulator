//! Hardware endpoints — the serial ports this machine offers, and the bridge that puts the
//! simulation on one of them.
//!
//! Kept apart from `simulation` because the concerns are different: that module is about a
//! vehicle, this one is about the wire it is reachable on.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Serialize;

use crate::AppState;

/// One serial port a user could connect to.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortDto {
    /// What to pass back to open it, e.g. `/dev/tty.usbmodem1101` or `COM3`.
    pub name: String,
    /// A hint about what is plugged in, when the operating system offers one.
    pub description: String,
}

/// What `GET /hw/ports` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortsDto {
    /// False when this build has no serial support compiled in, in which case `ports` is empty
    /// for that reason rather than because nothing is plugged in — a distinction a user
    /// staring at an empty list needs.
    pub serial_supported: bool,
    pub ports: Vec<SerialPortDto>,
}

/// GET /hw/ports — the serial ports this machine offers.
pub async fn GetSerialPorts(State(_state): State<Arc<AppState>>) -> Json<SerialPortsDto> {
    let vecPorts: Vec<SerialPortDto> = serial_can::ListPorts()
        .into_iter()
        .map(|port| SerialPortDto {
            name: port.m_strName,
            description: port.m_strDescription,
        })
        .collect();

    tracing::debug!(ports = vecPorts.len(), "listed serial ports");
    Json(SerialPortsDto {
        serial_supported: cfg!(feature = "serial"),
        ports: vecPorts,
    })
}
