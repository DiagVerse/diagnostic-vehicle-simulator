//! Simulation service — holds one loaded vehicle and answers UDS requests routed by CAN
//! address.
//!
//! This is the MVP's central use case: a CAN log is reconstructed into a Unified Vehicle
//! Model, every ECU in that model is started as a stateful [`VirtualEcu`], and inbound
//! requests are dispatched to the ECU that owns the request CAN identifier. The same service
//! serves both the virtual path (HTTP request from the UI) and, later, the hardware path
//! (frames off a real CAN bus) — the simulation is state-aware, never a log replay
//! (README §13).
//!
//! It lives in its own crate rather than in `application` because it needs the `ecu` runtime,
//! and `ecu` already depends on `application` for the `ProtocolHandler` port; putting the
//! service in `application` would form a dependency cycle.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeMap;

use application::ProtocolHandler;
use core_domain::model::{Ecu, Vehicle};
use ecu::VirtualEcu;

/// Errors from loading or driving a simulation.
#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// The CAN log could not be parsed or reconstructed.
    #[error("failed to reconstruct a vehicle from the log: {0}")]
    Reconstruct(#[from] reconstruct::ReconstructError),

    /// The log parsed, but no diagnostic ECU could be identified in it.
    #[error("no diagnostic ECU found in the log (no correlated UDS request/response pairs)")]
    NoEcusFound,

    /// The model contains an ECU without CAN addressing, so it cannot be routed to.
    #[error("ECU '{strEcuName}' has no CAN address and cannot be simulated on CAN")]
    MissingCanAddress {
        /// Name of the offending ECU.
        strEcuName: String,
    },

    /// Two ECUs claim the same request identifier, so routing would be ambiguous.
    #[error(
        "request CAN id 0x{u32RequestCanId:03X} is claimed by both '{strFirstEcu}' and '{strSecondEcu}'"
    )]
    DuplicateRequestCanId {
        /// The contested request identifier.
        u32RequestCanId: u32,
        /// ECU that claimed the identifier first.
        strFirstEcu: String,
        /// ECU that claimed it second.
        strSecondEcu: String,
    },
}

/// One ECU's answer to a routed request.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedResponse {
    /// Which ECU answered.
    pub m_strEcuName: String,
    /// The CAN identifier the answer is sent on.
    pub m_u32ResponseCanId: u32,
    /// The UDS response bytes. Empty when the ECU deliberately suppressed its response
    /// (suppressPosRspMsgIndicationBit) — the caller must then transmit nothing.
    pub m_vecResponse: Vec<u8>,
}

impl RoutedResponse {
    /// True when the ECU processed the request but deliberately sent nothing back.
    pub fn IsSuppressed(&self) -> bool {
        self.m_vecResponse.is_empty()
    }
}

/// The result of routing one inbound request.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingOutcome {
    /// No loaded ECU listens on that request identifier. A real ECU is silent on an
    /// identifier it does not own — it does **not** answer with a negative response — so the
    /// simulator stays silent too.
    NoTarget,
    /// One or more ECUs handled the request, in ECU order.
    Handled(Vec<RoutedResponse>),
}

/// A running simulation: the loaded vehicle plus one live ECU per CAN request identifier.
pub struct SimulationService {
    /// The model the running ECUs were built from; kept so the API can describe what is loaded.
    m_optVehicle: Option<Vehicle>,
    /// Live ECUs keyed by the CAN identifier a tester addresses them on. A `BTreeMap` keeps
    /// listing order stable and makes routing an O(log n) lookup.
    m_mapEcusByRequestId: BTreeMap<u32, VirtualEcu>,
}

impl Default for SimulationService {
    fn default() -> Self {
        SimulationService::New()
    }
}

impl SimulationService {
    /// Create an empty simulation with nothing loaded.
    pub fn New() -> Self {
        SimulationService {
            m_optVehicle: None,
            m_mapEcusByRequestId: BTreeMap::new(),
        }
    }

    /// Reconstruct a vehicle from CAN-log text and start every ECU in it.
    ///
    /// On success the previously loaded vehicle (and all its ECU state) is replaced. On
    /// failure the existing simulation is left untouched, so a bad upload cannot destroy a
    /// working session.
    pub fn LoadFromLogText(&mut self, strLogText: &str) -> Result<&Vehicle, SimulationError> {
        let vehicle = reconstruct::ReconstructFromLogText(strLogText)?;
        self.LoadVehicle(vehicle)
    }

    /// Start every ECU of an already-built vehicle model.
    pub fn LoadVehicle(&mut self, vehicle: Vehicle) -> Result<&Vehicle, SimulationError> {
        let mapEcus = BuildEcuMap(&vehicle)?;

        tracing::info!(
            vehicle = %vehicle.m_strName,
            ecus = mapEcus.len(),
            "simulation loaded"
        );
        for (u32RequestCanId, runningEcu) in &mapEcus {
            tracing::info!(
                ecu = %runningEcu.Config().m_strName,
                requestCanId = format!("{u32RequestCanId:03X}"),
                "ECU started"
            );
        }

        self.m_mapEcusByRequestId = mapEcus;
        self.m_optVehicle = Some(vehicle);
        Ok(self
            .m_optVehicle
            .as_ref()
            .expect("vehicle was just assigned"))
    }

    /// Discard the loaded vehicle and stop all ECUs.
    pub fn Clear(&mut self) {
        self.m_optVehicle = None;
        self.m_mapEcusByRequestId.clear();
        tracing::info!("simulation cleared");
    }

    /// The loaded vehicle model, or `None` if nothing is loaded.
    pub fn Vehicle(&self) -> Option<&Vehicle> {
        self.m_optVehicle.as_ref()
    }

    /// True when a vehicle is loaded and at least one ECU is running.
    pub fn IsLoaded(&self) -> bool {
        self.m_optVehicle.is_some()
    }

    /// Every running ECU, in request-identifier order, as (requestCanId, ECU) pairs.
    pub fn RunningEcus(&self) -> impl Iterator<Item = (u32, &VirtualEcu)> {
        self.m_mapEcusByRequestId
            .iter()
            .map(|(u32RequestCanId, runningEcu)| (*u32RequestCanId, runningEcu))
    }

    /// Look up one running ECU by the identifier it is addressed on.
    pub fn FindEcuByRequestCanId(&self, u32RequestCanId: u32) -> Option<&VirtualEcu> {
        self.m_mapEcusByRequestId.get(&u32RequestCanId)
    }

    /// Restart every running ECU: back to the default session with security locked. The
    /// loaded model is kept.
    pub fn ResetAllEcus(&mut self) {
        for runningEcu in self.m_mapEcusByRequestId.values_mut() {
            *runningEcu = VirtualEcu::New(runningEcu.Config().clone());
        }
        tracing::info!(
            ecus = self.m_mapEcusByRequestId.len(),
            "all ECUs reset to default session"
        );
    }

    /// Route one inbound UDS request, addressed to `u32RequestCanId`, to the ECU that owns
    /// that identifier and return its answer.
    ///
    /// Physical addressing only: exactly one ECU owns a given request identifier. An
    /// identifier no ECU owns yields [`RoutingOutcome::NoTarget`] and no response at all.
    pub fn ProcessByCanId(
        &mut self,
        u32RequestCanId: u32,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        let runningEcu = match self.m_mapEcusByRequestId.get_mut(&u32RequestCanId) {
            Some(runningEcu) => runningEcu,
            None => {
                tracing::warn!(
                    requestCanId = format!("{u32RequestCanId:03X}"),
                    "no ECU listens on this CAN id; staying silent"
                );
                return RoutingOutcome::NoTarget;
            }
        };

        let response = ProcessOnEcu(runningEcu, u32RequestCanId, vecRequest, protocol);
        RoutingOutcome::Handled(vec![response])
    }
}

/// Drive one ECU with a request and package its answer for the caller.
fn ProcessOnEcu(
    runningEcu: &mut VirtualEcu,
    u32RequestCanId: u32,
    vecRequest: &[u8],
    protocol: &dyn ProtocolHandler,
) -> RoutedResponse {
    // Every routed ECU has a CAN address — `BuildEcuMap` rejects the model otherwise — so
    // reading it back here cannot fail.
    let u32ResponseCanId = runningEcu
        .Config()
        .m_optCanAddress
        .expect("a routed ECU always has a CAN address; BuildEcuMap enforces it")
        .m_u32ResponseCanId;
    let strEcuName = runningEcu.Config().m_strName.clone();

    let vecResponse = runningEcu.ProcessRequest(protocol, vecRequest);

    if vecResponse.is_empty() {
        tracing::info!(
            ecu = %strEcuName,
            requestCanId = format!("{u32RequestCanId:03X}"),
            "response suppressed by the ECU"
        );
    } else {
        tracing::info!(
            ecu = %strEcuName,
            requestCanId = format!("{u32RequestCanId:03X}"),
            responseCanId = format!("{u32ResponseCanId:03X}"),
            responseBytes = vecResponse.len(),
            "request answered"
        );
    }

    RoutedResponse {
        m_strEcuName: strEcuName,
        m_u32ResponseCanId: u32ResponseCanId,
        m_vecResponse: vecResponse,
    }
}

/// Build the request-identifier → running-ECU map, rejecting models that cannot be routed.
fn BuildEcuMap(vehicle: &Vehicle) -> Result<BTreeMap<u32, VirtualEcu>, SimulationError> {
    if vehicle.m_vecEcus.is_empty() {
        return Err(SimulationError::NoEcusFound);
    }

    let mut mapEcus: BTreeMap<u32, VirtualEcu> = BTreeMap::new();
    for config in &vehicle.m_vecEcus {
        let u32RequestCanId = RequestCanIdOf(config)?;

        if let Some(existing) = mapEcus.get(&u32RequestCanId) {
            return Err(SimulationError::DuplicateRequestCanId {
                u32RequestCanId,
                strFirstEcu: existing.Config().m_strName.clone(),
                strSecondEcu: config.m_strName.clone(),
            });
        }

        mapEcus.insert(u32RequestCanId, VirtualEcu::New(config.clone()));
    }

    Ok(mapEcus)
}

/// The CAN identifier an ECU is addressed on, or an error if the model does not record one.
fn RequestCanIdOf(config: &Ecu) -> Result<u32, SimulationError> {
    match config.m_optCanAddress {
        Some(address) => Ok(address.m_u32RequestCanId),
        None => Err(SimulationError::MissingCanAddress {
            strEcuName: config.m_strName.clone(),
        }),
    }
}
