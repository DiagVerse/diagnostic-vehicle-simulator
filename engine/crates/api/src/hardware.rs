//! Hardware endpoints — the serial ports this machine offers, and the bridge that puts the
//! simulation on one of them.
//!
//! Kept apart from `simulation` because the concerns are different: that module is about a
//! vehicle, this one is about the wire it is reachable on.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::{Arc, Mutex};

use axum::{extract::State, Json};
use bridge::probe::{DetectSerialLinkSpeed, RequestedSerialLinkSpeed, SerialLinkSpeed};
use bridge::{BridgeStats, CanBridge};
use can::busload::{BusLoadMeter, BusLoadSample};
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
    /// The host-to-adapter line speed in use, and how that number was arrived at. `None` until
    /// a link has been opened.
    pub m_optLinkSpeed: Option<SerialLinkSpeed>,
    /// Its frame counters.
    pub m_optStats: Option<Arc<BridgeStats>>,
    /// Its bus-load history, shared with the running bridge.
    pub m_optBusLoad: Option<Arc<Mutex<BusLoadMeter>>>,
}

/// Request body for `POST /hw/start`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartHardwareBody {
    /// A port name from `GET /hw/ports`, or a virtual one such as a PTY.
    pub port: String,
    /// Bus speed in bits per second. Must be one an adapter can select.
    pub bitrate_bps: u32,
    /// Host-to-adapter line speed in bits per second — **not** the CAN bitrate.
    ///
    /// Omit it and the adapter is asked what it runs at, which is the right thing almost
    /// always. Name one to skip the probe: to save the second it takes, or because the adapter
    /// runs a rate the probe does not try.
    #[serde(default)]
    pub serial_baud_bps: Option<u32>,
}

/// What `GET /hw/status` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareStatusDto {
    pub running: bool,
    pub port: Option<String>,
    pub bitrate_bps: Option<u32>,
    /// Host-to-adapter line speed, not the CAN bitrate. The pair below say where it came from,
    /// because a measured number and an assumed one call for different next steps when long
    /// requests start arriving with holes in them.
    pub serial_baud_bps: Option<u32>,
    pub serial_baud_source: Option<String>,
    /// What the adapter called itself when it answered the probe, if it did.
    pub adapter_version: Option<String>,
    pub frames_received: u64,
    pub frames_sent: u64,
}

/// Request body for `POST /hw/line-speed`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandLineSpeedBody {
    /// A port name from `GET /hw/ports`.
    pub port: String,
    /// The host-to-adapter line speed to ask the adapter to switch to — **not** the CAN
    /// bitrate. Must be one the `U` command can name.
    pub serial_baud_bps: u32,
    /// The speed the adapter is on now, so the command can be sent at a rate it is listening
    /// at. Omit it and the adapter is probed first.
    #[serde(default)]
    pub current_serial_baud_bps: Option<u32>,
}

/// What `POST /hw/line-speed` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineSpeedChangeDto {
    /// `confirmed`, `refused`, `notConfirmed` or `noRealSerialLine`.
    pub outcome: String,
    /// The speed the link is on now, whatever happened.
    pub serial_baud_bps: u32,
    /// The speed that was asked for, which is not the same thing unless it worked.
    pub requested_serial_baud_bps: u32,
    /// What the adapter called itself at the new speed, when it answered.
    pub adapter_version: Option<String>,
    /// The outcome in words, for an operator deciding whether to try a slower speed or stop.
    pub message: String,
}

/// POST /hw/line-speed — ask the adapter to run its UART faster.
///
/// This is the one thing probing cannot do. `GET /hw/status` reports the speed an adapter is
/// *already* using; this changes it, which on a link that is the bottleneck by a factor of
/// eight is the cheapest improvement available — and the only one that costs nothing on the
/// CAN bus itself.
///
/// **Never automatic, and deliberately so.** On several firmwares the setting survives a power
/// cycle, so an adapter left at a raised speed will not talk to other software that assumes
/// 115200 until it is set back. A simulator that silently reconfigured a shared piece of
/// hardware would be making a decision that is not its to make.
///
/// Refused while a bridge is running: the live link holds the port, and changing the speed
/// underneath it would leave the bridge talking at a rate the adapter has left.
pub async fn PostCommandLineSpeed(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CommandLineSpeedBody>,
) -> Result<Json<LineSpeedChangeDto>, ApiError> {
    let speed = slcan::SlcanLineSpeed::FromBitsPerSecond(body.serial_baud_bps).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "{} baud is not a line speed the SLCAN U command can select; use 230400, 115200, 57600, 38400, 19200, 9600 or 2400",
            body.serial_baud_bps
        ))
    })?;

    {
        let hardware = state.hardware.lock().expect("hardware mutex poisoned");
        if hardware.m_optTask.is_some() {
            return Err(ApiError::Conflict(format!(
                "a bridge is running on {}; stop it before changing the adapter's line speed, or it will be left talking at a rate the adapter has left",
                hardware.m_strPortName
            )));
        }
    }

    // Probed rather than assumed when the caller does not say: sending the command at a speed
    // the adapter is not listening at achieves nothing and reports nothing useful.
    let u32CurrentBaudRate = match body.current_serial_baud_bps {
        Some(u32Current) => u32Current,
        None => DetectSerialLinkSpeed(&body.port).m_u32BaudRate,
    };

    let change = bridge::probe::CommandLineSpeed(&body.port, u32CurrentBaudRate, speed);
    let strOutcome = match &change {
        bridge::probe::LineSpeedChange::Confirmed { .. } => "confirmed",
        bridge::probe::LineSpeedChange::Refused { .. } => "refused",
        bridge::probe::LineSpeedChange::NotConfirmed { .. } => "notConfirmed",
        bridge::probe::LineSpeedChange::NoRealSerialLine => "noRealSerialLine",
    };
    let optStrAdapterVersion = match &change {
        bridge::probe::LineSpeedChange::Confirmed {
            strAdapterVersion, ..
        } => Some(strAdapterVersion.clone()),
        _ => None,
    };

    Ok(Json(LineSpeedChangeDto {
        outcome: strOutcome.to_string(),
        serial_baud_bps: change.EffectiveBaudRate(u32CurrentBaudRate),
        requested_serial_baud_bps: body.serial_baud_bps,
        adapter_version: optStrAdapterVersion,
        message: change.Describe(),
    }))
}

/// Work out the host-to-adapter line speed for this connection.
///
/// This is the rate between host and dongle, not the CAN bitrate. They are set separately and
/// only one of them appears on the bus — a distinction worth keeping straight, because getting
/// them confused produces a link that looks open and carries nothing.
///
/// It used to be a constant 115200. It is determined per connection now because the constant
/// was wrong in a way that was invisible until a long request arrived with holes in it: the
/// line, not the bus, was the bottleneck, and nothing reported that.
fn ResolveSerialLinkSpeed(strPortName: &str, optU32Requested: Option<u32>) -> SerialLinkSpeed {
    match optU32Requested {
        Some(u32Requested) => RequestedSerialLinkSpeed(u32Requested),
        None => DetectSerialLinkSpeed(strPortName),
    }
}

/// One second of bus activity, as the UI plots it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusLoadSampleDto {
    pub at_sec: f64,
    pub frames: usize,
    /// Share of the bus occupied, 0.0–1.0, with bit stuffing excluded.
    pub load_nominal: f64,
    /// The same with every frame stuffed as heavily as the standard allows.
    pub load_worst_case: f64,
}

/// What `GET /hw/busload` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusLoadDto {
    /// The bitrate load is measured against. Zero when no link is open, in which case the
    /// series is empty rather than misleading.
    pub bitrate_bps: u32,
    /// One entry per second, oldest first, at most two minutes of history.
    pub samples: Vec<BusLoadSampleDto>,
    /// The second currently being filled — incomplete by definition, so reported apart from
    /// the finished ones rather than plotted as though it were one of them.
    pub current: Option<BusLoadSampleDto>,
    /// The busiest finished second in the history.
    pub peak: Option<BusLoadSampleDto>,
    /// True while a load figure is a floor rather than a measurement, which it always is: bit
    /// stuffing cannot be recovered from a decoded frame, so the honest answer is a range.
    pub is_nominal_a_floor: bool,
}

/// GET /hw/busload — how busy the bus has been, second by second.
pub async fn GetBusLoad(State(state): State<Arc<AppState>>) -> Json<BusLoadDto> {
    let hardware = state.hardware.lock().expect("hardware mutex poisoned");

    let arcMeter = match hardware.m_optBusLoad.as_ref() {
        Some(arcMeter) => arcMeter,
        // No link open: an empty series, not a flat line at zero, which would claim a quiet bus
        // was observed when nothing was observed at all.
        None => {
            return Json(BusLoadDto {
                bitrate_bps: 0,
                samples: Vec::new(),
                current: None,
                peak: None,
                is_nominal_a_floor: true,
            })
        }
    };

    let meter = arcMeter.lock().expect("bus load mutex poisoned");
    Json(BusLoadDto {
        bitrate_bps: meter.BitrateBps(),
        samples: meter.Samples().iter().map(BuildSampleDto).collect(),
        current: meter.Current().as_ref().map(BuildSampleDto),
        peak: meter.Peak().as_ref().map(BuildSampleDto),
        is_nominal_a_floor: true,
    })
}

fn BuildSampleDto(sample: &BusLoadSample) -> BusLoadSampleDto {
    BusLoadSampleDto {
        at_sec: sample.m_f64AtSec,
        frames: sample.m_uFrames,
        load_nominal: sample.m_f64LoadNominal,
        load_worst_case: sample.m_f64LoadWorstCase,
    }
}

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

    // Probing opens and closes the port several times, so it happens before the live one is
    // opened rather than while it is held.
    let linkSpeed = ResolveSerialLinkSpeed(&body.port, body.serial_baud_bps);

    let boxTransport = serial_can::OpenPort(&body.port, linkSpeed.m_u32BaudRate)
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
    .WithObserver(Arc::new(state.traffic.clone()))
    // The bridge needs both speeds to know whether an ECU asking for no pacing is asking for
    // something this link can actually deliver.
    .WithLinkCapacity(linkSpeed.m_u32BaudRate, body.bitrate_bps);
    let arcStats = canBridge.Stats();
    let arcBusLoad = canBridge.BusLoad();

    let task = tokio::spawn(async move {
        canBridge.Run(&protocol).await;
    });

    hardware.m_optTask = Some(task);
    hardware.m_strPortName = body.port.clone();
    hardware.m_u32BitrateBps = body.bitrate_bps;
    hardware.m_optLinkSpeed = Some(linkSpeed.clone());
    hardware.m_optStats = Some(arcStats);
    hardware.m_optBusLoad = Some(arcBusLoad);

    tracing::info!(
        port = %body.port,
        bitrate = body.bitrate_bps,
        serialBaud = linkSpeed.m_u32BaudRate,
        serialBaudSource = linkSpeed.m_source.Describe(),
        "bridging the simulation onto a CAN bus"
    );
    state.traffic.Publish(TrafficEvent::Lifecycle {
        at_ms: NowMs(),
        what: format!(
            "on the wire: {} at {} bit/s, host link {} baud ({})",
            body.port,
            body.bitrate_bps,
            linkSpeed.m_u32BaudRate,
            linkSpeed.m_source.Describe()
        ),
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
    hardware.m_optLinkSpeed = None;
    hardware.m_optBusLoad = None;

    Json(BuildStatusDto(&hardware))
}

/// Describe whatever the bridge is doing.
fn BuildStatusDto(hardware: &HardwareState) -> HardwareStatusDto {
    let bIsRunning = hardware.m_optTask.is_some();
    HardwareStatusDto {
        running: bIsRunning,
        port: bIsRunning.then(|| hardware.m_strPortName.clone()),
        bitrate_bps: bIsRunning.then_some(hardware.m_u32BitrateBps),
        serial_baud_bps: hardware
            .m_optLinkSpeed
            .as_ref()
            .filter(|_| bIsRunning)
            .map(|speed| speed.m_u32BaudRate),
        serial_baud_source: hardware
            .m_optLinkSpeed
            .as_ref()
            .filter(|_| bIsRunning)
            .map(|speed| speed.m_source.Describe().to_string()),
        adapter_version: hardware
            .m_optLinkSpeed
            .as_ref()
            .filter(|_| bIsRunning)
            .and_then(|speed| speed.m_optStrAdapterVersion.clone()),
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
