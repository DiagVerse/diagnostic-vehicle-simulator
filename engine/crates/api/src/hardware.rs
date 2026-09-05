//! Hardware endpoints — the serial ports this machine offers, and the bridge that puts the
//! simulation on one of them.
//!
//! Kept apart from `simulation` because the concerns are different: that module is about a
//! vehicle, this one is about the wire it is reachable on.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::Arc;

use axum::{extract::State, Json};
use bridge::{BridgeStats, CanBridge};
use isotp::params::IsoTpParameters;
use serde::{Deserialize, Serialize};
use slcan::SlcanBitrate;
use tokio::task::JoinHandle;

use crate::simulation::ApiError;
use crate::traffic::{NowMs, TrafficEvent};
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

/// What the engine knows about a running bridge.
#[derive(Default)]
pub struct HardwareState {
    /// The task pumping the bus, if one is running.
    pub m_optTask: Option<JoinHandle<()>>,
    /// The port it opened.
    pub m_strPortName: String,
    /// The bitrate it opened at.
    pub m_u32BitrateBps: u32,
    /// Its frame counters.
    pub m_optStats: Option<Arc<BridgeStats>>,
}

/// Request body for `POST /hw/start`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartHardwareBody {
    /// A port name from `GET /hw/ports`, or a virtual one such as a PTY.
    pub port: String,
    /// Bus speed in bits per second. Must be one an adapter can select.
    pub bitrate_bps: u32,
}

/// What `GET /hw/status` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareStatusDto {
    pub running: bool,
    pub port: Option<String>,
    pub bitrate_bps: Option<u32>,
    pub frames_received: u64,
    pub frames_sent: u64,
}

/// The serial line speed used to reach an adapter.
///
/// This is the rate between host and dongle, not the CAN bitrate. They are set separately and
/// only one of them appears on the bus — a distinction worth keeping straight, because getting
/// them confused produces a link that looks open and carries nothing.
const c_u32SerialBaudRate: u32 = 115_200;

/// GET /hw/status — whether the simulation is on a wire, and how much has crossed it.
pub async fn GetHardwareStatus(State(state): State<Arc<AppState>>) -> Json<HardwareStatusDto> {
    let hardware = state.hardware.lock().expect("hardware mutex poisoned");
    Json(BuildStatusDto(&hardware))
}

/// POST /hw/start — open a port and put the simulation on it.
pub async fn PostHardwareStart(
    State(state): State<Arc<AppState>>,
    Json(body): Json<StartHardwareBody>,
) -> Result<Json<HardwareStatusDto>, ApiError> {
    let bitrate = SlcanBitrate::FromBitsPerSecond(body.bitrate_bps).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "{} bit/s is not a rate an SLCAN adapter can select; use 10000, 20000, 50000, 100000, 125000, 250000, 500000, 800000 or 1000000",
            body.bitrate_bps
        ))
    })?;

    // Cloned rather than borrowed: the bridge runs for as long as the link is up, so it owns
    // its own handle instead of holding a reference into shared state.
    let protocol = state.protocol.clone().ok_or_else(|| {
        ApiError::Conflict(
            "UDS protocol plugin is not loaded (place libuds_plugin.* in plugins.d/)".to_string(),
        )
    })?;

    let mut hardware = state.hardware.lock().expect("hardware mutex poisoned");
    if hardware.m_optTask.is_some() {
        return Err(ApiError::Conflict(format!(
            "already bridging on {}; stop it first",
            hardware.m_strPortName
        )));
    }

    let boxTransport = serial_can::OpenPort(&body.port, c_u32SerialBaudRate)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let bus = bridge::bus::SlcanBus::Open(boxTransport, bitrate)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;

    // The traffic channel watches the wire, so every frame a real tester exchanges with the
    // simulation reaches the monitor. Without it the only evidence of a whole session is two
    // counters going up.
    let mut canBridge = CanBridge::New(
        Box::new(bus),
        Arc::clone(&state.simulation),
        IsoTpParameters::default(),
    )
    .WithObserver(Arc::new(state.traffic.clone()));
    let arcStats = canBridge.Stats();

    let task = tokio::spawn(async move {
        canBridge.Run(&protocol).await;
    });

    hardware.m_optTask = Some(task);
    hardware.m_strPortName = body.port.clone();
    hardware.m_u32BitrateBps = body.bitrate_bps;
    hardware.m_optStats = Some(arcStats);

    tracing::info!(port = %body.port, bitrate = body.bitrate_bps, "bridging the simulation onto a CAN bus");
    state.traffic.Publish(TrafficEvent::Lifecycle {
        at_ms: NowMs(),
        what: format!("on the wire: {} at {} bit/s", body.port, body.bitrate_bps),
    });
    Ok(Json(BuildStatusDto(&hardware)))
}

/// POST /hw/stop — take the simulation off the wire.
pub async fn PostHardwareStop(State(state): State<Arc<AppState>>) -> Json<HardwareStatusDto> {
    let mut hardware = state.hardware.lock().expect("hardware mutex poisoned");

    if let Some(task) = hardware.m_optTask.take() {
        task.abort();
        tracing::info!(port = %hardware.m_strPortName, "stopped bridging");
    }
    hardware.m_optStats = None;
    hardware.m_strPortName.clear();
    hardware.m_u32BitrateBps = 0;

    Json(BuildStatusDto(&hardware))
}

/// Describe whatever the bridge is doing.
fn BuildStatusDto(hardware: &HardwareState) -> HardwareStatusDto {
    let bIsRunning = hardware.m_optTask.is_some();
    HardwareStatusDto {
        running: bIsRunning,
        port: bIsRunning.then(|| hardware.m_strPortName.clone()),
        bitrate_bps: bIsRunning.then_some(hardware.m_u32BitrateBps),
        frames_received: hardware
            .m_optStats
            .as_ref()
            .map(|stats| stats.FramesReceived())
            .unwrap_or(0),
        frames_sent: hardware
            .m_optStats
            .as_ref()
            .map(|stats| stats.FramesSent())
            .unwrap_or(0),
    }
}
