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
use core_domain::model::{CanAddress, Ecu, EcuTiming, Vehicle};
use ecu::schedule::ResponsePlan;
use ecu::VirtualEcu;

/// First byte of a UDS negative response (ISO 14229-1).
const c_byNegativeResponseSid: u8 = 0x7F;

/// Negative response codes a server must suppress when the request was functionally addressed
/// (ISO 14229-1 clause 7.5.3.3 Table 5 and clause 7.5.4.3 Table 7). Every other NRC is sent
/// normally.
const c_arrFunctionallySuppressedNrcs: [u8; 5] = [
    0x11, // serviceNotSupported
    0x12, // sub-functionNotSupported
    0x31, // requestOutOfRange
    0x7E, // sub-functionNotSupportedInActiveSession
    0x7F, // serviceNotSupportedInActiveSession
];

/// Longest functional request the simulator accepts. A broadcast has no single peer to send
/// flow control, so ISO 15765-2 permits a SingleFrame only — seven payload bytes with normal
/// addressing.
const c_uMaxFunctionalRequestBytes: usize = 7;

/// Errors from loading or driving a simulation.
#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// The CAN log could not be parsed or reconstructed.
    #[error("failed to reconstruct a vehicle from the log: {0}")]
    Reconstruct(#[from] reconstruct::ReconstructError),

    /// The log parsed, but no diagnostic ECU could be identified in it.
    #[error("no diagnostic ECU found in the log (no correlated UDS request/response pairs)")]
    NoEcusFound,

    /// Not one ECU in the model carries CAN addressing, so nothing can be routed to.
    #[error("no ECU in the model has a CAN address; none can be simulated on CAN")]
    NoRoutableEcus,

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

    /// Two ECUs answer on the same identifier, so their responses would be indistinguishable
    /// on the wire.
    #[error(
        "response CAN id 0x{u32ResponseCanId:03X} is claimed by both '{strFirstEcu}' and '{strSecondEcu}'"
    )]
    DuplicateResponseCanId {
        /// The contested response identifier.
        u32ResponseCanId: u32,
        /// ECU that claimed the identifier first.
        strFirstEcu: String,
        /// ECU that claimed it second.
        strSecondEcu: String,
    },

    /// The caller tried to change a vehicle before there was one.
    #[error("no vehicle is loaded; create one or load a CAN log first")]
    NoVehicleLoaded,

    /// An ECU was submitted without the CAN addressing it needs to be reachable.
    #[error("ECU '{strEcuName}' has no CAN address, so nothing could reach it")]
    MissingCanAddress {
        /// Name of the offending ECU.
        strEcuName: String,
    },

    /// No running ECU is addressed by the given identifier.
    #[error("no ECU is addressed on CAN id 0x{u32RequestCanId:03X}")]
    EcuNotFound {
        /// The identifier that matched nothing.
        u32RequestCanId: u32,
    },

    /// One identifier is both a broadcast address and an ECU's own request address, which
    /// would make routing ambiguous.
    #[error(
        "CAN id 0x{u32CanId:03X} is both '{strEcuName}'s own request identifier and a functional (broadcast) identifier"
    )]
    FunctionalIdCollidesWithPhysical {
        /// The contested identifier.
        u32CanId: u32,
        /// The ECU whose physical request identifier it is.
        strEcuName: String,
    },
}

/// One ECU's answer to a routed request.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedResponse {
    /// Which ECU answered.
    pub m_strEcuName: String,
    /// The identifier that ECU is physically addressed on. For a broadcast this is the ECU's
    /// own identifier, not the broadcast one, so a caller can tell exactly which ECUs a
    /// request occupied.
    pub m_u32RequestCanId: u32,
    /// The CAN identifier the answer is sent on.
    pub m_u32ResponseCanId: u32,
    /// The UDS response bytes. Empty when the ECU deliberately suppressed its response
    /// (suppressPosRspMsgIndicationBit) — the caller must then transmit nothing.
    pub m_vecResponse: Vec<u8>,
    /// The ECU's session after the request. A suppressed response still changes state, so this
    /// is reported even when nothing was sent.
    pub m_bySession: u8,
    /// Whether the ECU has a security level unlocked after the request.
    pub m_bIsSecurityUnlocked: bool,
    /// The full timed answer: any ResponsePending messages, the final response, and when each
    /// goes on the wire. The caller executes it; nothing here sleeps.
    pub m_plan: ResponsePlan,
}

impl RoutedResponse {
    /// True when the ECU processed the request but nothing goes on the wire — either the
    /// positive response was suppressed, or fault injection withheld it. `m_plan` says which.
    pub fn IsSuppressed(&self) -> bool {
        self.m_vecResponse.is_empty()
    }

    /// True when this answer began with one or more ResponsePending messages.
    pub fn HasResponsePending(&self) -> bool {
        self.m_plan.m_u8ResponsePendingCount > 0
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
    /// For each functional (broadcast) identifier, the physical request identifiers of the
    /// ECUs that listen on it, in ascending response-identifier order.
    m_mapFunctionalTargets: BTreeMap<u32, Vec<u32>>,
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
            m_mapFunctionalTargets: BTreeMap::new(),
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
        let mapFunctionalTargets = BuildFunctionalTargetMap(&mapEcus)?;

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
        self.m_mapFunctionalTargets = mapFunctionalTargets;
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
        self.m_mapFunctionalTargets.clear();
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

    /// True when the identifier is a functional (broadcast) address some running ECU listens
    /// on, rather than one ECU's own request address.
    pub fn IsFunctionalCanId(&self, u32CanId: u32) -> bool {
        self.m_mapFunctionalTargets.contains_key(&u32CanId)
    }

    /// Look up one running ECU by the identifier it is addressed on.
    pub fn FindEcuByRequestCanId(&self, u32RequestCanId: u32) -> Option<&VirtualEcu> {
        self.m_mapEcusByRequestId.get(&u32RequestCanId)
    }

    /// Start an empty vehicle to build up by hand.
    ///
    /// Unlike a reconstruction, an empty vehicle is a legitimate starting point: the user is
    /// about to add ECUs one at a time. Any previously loaded vehicle is replaced.
    pub fn CreateEmptyVehicle(&mut self, strName: &str) -> &Vehicle {
        self.m_mapEcusByRequestId.clear();
        self.m_mapFunctionalTargets.clear();
        self.m_optVehicle = Some(Vehicle {
            m_strName: strName.to_string(),
            m_vecEcus: Vec::new(),
        });

        tracing::info!(vehicle = %strName, "empty vehicle created");
        self.m_optVehicle
            .as_ref()
            .expect("vehicle was just assigned")
    }

    /// Add one ECU to the loaded vehicle and start it.
    ///
    /// Rejected if its identifiers collide with an ECU already running, because routing would
    /// then be ambiguous — the same check a reconstructed model gets at load time.
    pub fn AddEcu(&mut self, config: Ecu) -> Result<(), SimulationError> {
        if self.m_optVehicle.is_none() {
            return Err(SimulationError::NoVehicleLoaded);
        }

        let address = config
            .m_optCanAddress
            .ok_or_else(|| SimulationError::MissingCanAddress {
                strEcuName: config.m_strName.clone(),
            })?;

        RejectDuplicateIdentifiers(&self.m_mapEcusByRequestId, &config, &address)?;

        tracing::info!(
            ecu = %config.m_strName,
            requestCanId = format!("{:03X}", address.m_u32RequestCanId),
            responseCanId = format!("{:03X}", address.m_u32ResponseCanId),
            "ECU added"
        );

        self.m_mapEcusByRequestId
            .insert(address.m_u32RequestCanId, VirtualEcu::New(config.clone()));
        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            vehicle.m_vecEcus.push(config);
        }

        self.RebuildFunctionalTargets()
    }

    /// Remove one ECU and stop it.
    pub fn RemoveEcu(&mut self, u32RequestCanId: u32) -> Result<(), SimulationError> {
        let removed = self
            .m_mapEcusByRequestId
            .remove(&u32RequestCanId)
            .ok_or(SimulationError::EcuNotFound { u32RequestCanId })?;

        tracing::info!(ecu = %removed.Config().m_strName, "ECU removed");

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            vehicle
                .m_vecEcus
                .retain(|config| !IsAddressedOn(config, u32RequestCanId));
        }

        self.RebuildFunctionalTargets()
    }

    /// Rename one ECU.
    ///
    /// A reconstructed ECU is named after the identifier it answers on (`ECU_7E8`), which is
    /// accurate but tells a reader nothing. Naming it "Engine" is the single most useful thing
    /// a user can add to a reconstructed model.
    pub fn RenameEcu(
        &mut self,
        u32RequestCanId: u32,
        strName: &str,
    ) -> Result<(), SimulationError> {
        let runningEcu = self
            .m_mapEcusByRequestId
            .get_mut(&u32RequestCanId)
            .ok_or(SimulationError::EcuNotFound { u32RequestCanId })?;
        runningEcu.SetName(strName);

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            for config in &mut vehicle.m_vecEcus {
                if IsAddressedOn(config, u32RequestCanId) {
                    config.m_strName = strName.to_string();
                }
            }
        }

        Ok(())
    }

    /// Recompute which ECUs listen on each broadcast identifier, after the set of running ECUs
    /// has changed.
    fn RebuildFunctionalTargets(&mut self) -> Result<(), SimulationError> {
        self.m_mapFunctionalTargets = BuildFunctionalTargetMap(&self.m_mapEcusByRequestId)?;
        Ok(())
    }

    /// Replace one ECU's timing parameters.
    ///
    /// Written to both the running ECU and the loaded model, so the model JSON and what is
    /// actually simulated cannot drift apart. The caller validates the parameters first.
    ///
    /// The change applies to the **next** request: an answer already scheduled keeps the
    /// values it was built with, and the tester only learns new P2/P2* values at the next
    /// DiagnosticSessionControl response, which is the sole place ISO 14229-1 carries them.
    pub fn SetEcuTiming(
        &mut self,
        u32RequestCanId: u32,
        timing: EcuTiming,
    ) -> Result<(), SimulationError> {
        let runningEcu = self
            .m_mapEcusByRequestId
            .get_mut(&u32RequestCanId)
            .ok_or(SimulationError::EcuNotFound { u32RequestCanId })?;
        runningEcu.SetTiming(timing);

        UpdateVehicleTiming(self.m_optVehicle.as_mut(), u32RequestCanId, timing);
        Ok(())
    }

    /// One ECU's current timing parameters.
    pub fn EcuTimingOf(&self, u32RequestCanId: u32) -> Result<EcuTiming, SimulationError> {
        self.m_mapEcusByRequestId
            .get(&u32RequestCanId)
            .map(|runningEcu| runningEcu.Timing())
            .ok_or(SimulationError::EcuNotFound { u32RequestCanId })
    }

    /// Restart every running ECU: back to the default session with security locked. The
    /// loaded model is kept.
    /// A reset returns *diagnostic state* to default, not *configuration*: rebuilding from
    /// the ECU's own config carries operator-set timing across unchanged, which is what an
    /// operator who set up a fault and then reset the session expects.
    pub fn ResetAllEcus(&mut self) {
        for runningEcu in self.m_mapEcusByRequestId.values_mut() {
            *runningEcu = VirtualEcu::New(runningEcu.Config().clone());
        }
        tracing::info!(
            ecus = self.m_mapEcusByRequestId.len(),
            "all ECUs reset to default session"
        );
    }

    /// Route one inbound UDS request, addressed to `u32RequestCanId`, and return the answers.
    ///
    /// Three cases, kept as separate branches because they behave differently on the wire:
    ///   1. **Physical** — exactly one ECU owns the identifier; it answers on its own response
    ///      identifier.
    ///   2. **Functional (broadcast)** — every ECU listening on the identifier processes the
    ///      request on its own state and answers on its own response identifier. Answers come
    ///      back in ascending response-identifier order, which is the order CAN arbitration
    ///      would produce.
    ///   3. **Neither** — no ECU has the identifier in its acceptance filter, so no ECU even
    ///      receives the request. Nothing is transmitted; a negative response would imply a
    ///      server that decided to reject, and there is none.
    pub fn ProcessByCanId(
        &mut self,
        u32RequestCanId: u32,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        if self.m_mapEcusByRequestId.contains_key(&u32RequestCanId) {
            return self.ProcessPhysical(u32RequestCanId, vecRequest, protocol);
        }

        if self.m_mapFunctionalTargets.contains_key(&u32RequestCanId) {
            return self.ProcessFunctional(u32RequestCanId, vecRequest, protocol);
        }

        // A tester scanning for ECUs addresses identifiers nothing answers on; that is normal
        // traffic, not a fault, so it is logged at debug level.
        tracing::debug!(
            requestCanId = format!("{u32RequestCanId:03X}"),
            "no ECU listens on this CAN id; staying silent"
        );
        RoutingOutcome::NoTarget
    }

    /// Case 1: the identifier belongs to exactly one ECU.
    fn ProcessPhysical(
        &mut self,
        u32RequestCanId: u32,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        let runningEcu = match self.m_mapEcusByRequestId.get_mut(&u32RequestCanId) {
            Some(runningEcu) => runningEcu,
            None => return RoutingOutcome::NoTarget,
        };

        let response = ProcessOnEcu(runningEcu, u32RequestCanId, vecRequest, protocol);
        RoutingOutcome::Handled(vec![response])
    }

    /// Case 2: a broadcast identifier every listening ECU processes on its own state.
    fn ProcessFunctional(
        &mut self,
        u32FunctionalCanId: u32,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        // A functional request cannot be segmented: there is no single peer to send flow
        // control, so ISO 15765-2 allows a SingleFrame only.
        if vecRequest.len() > c_uMaxFunctionalRequestBytes {
            tracing::warn!(
                requestCanId = format!("{u32FunctionalCanId:03X}"),
                requestBytes = vecRequest.len(),
                maxBytes = c_uMaxFunctionalRequestBytes,
                "functional request is too long to be sent in a single frame; ignoring it"
            );
            return RoutingOutcome::NoTarget;
        }

        let vecTargetRequestIds = match self.m_mapFunctionalTargets.get(&u32FunctionalCanId) {
            Some(vecTargets) => vecTargets.clone(),
            None => return RoutingOutcome::NoTarget,
        };

        let mut vecResponses = Vec::new();
        for u32TargetRequestId in vecTargetRequestIds {
            let runningEcu = match self.m_mapEcusByRequestId.get_mut(&u32TargetRequestId) {
                Some(runningEcu) => runningEcu,
                None => continue,
            };

            let response = ProcessOnEcu(runningEcu, u32FunctionalCanId, vecRequest, protocol);

            // A functionally addressed server must stay silent for some negative responses
            // rather than flood the tester with "I do not support that" (ISO 14229-1
            // clause 7.5.3.3 Table 5 and clause 7.5.4.3 Table 7). The state change, if any,
            // has already been applied.
            //
            // Unless it already answered with a ResponsePending: having told the tester it is
            // there and working, going silent would strand it until P2* expires, so the final
            // negative response must be sent after all (ISO 14229-1 clause 7.5.5 and
            // Annex A.1).
            let bMustAnswerAfterPending = response.HasResponsePending();
            if !bMustAnswerAfterPending
                && IsNegativeResponseSuppressedFunctionally(&response.m_vecResponse)
            {
                tracing::debug!(
                    ecu = %response.m_strEcuName,
                    nrc = format!("{:02X}", NegativeResponseCodeOf(&response.m_vecResponse).unwrap_or(0)),
                    "negative response suppressed because the request was functionally addressed"
                );
                continue;
            }

            vecResponses.push(response);
        }

        // Ascending response identifier: CAN arbitration is won by the lower identifier, so
        // this is the order the answers would appear on a real bus.
        vecResponses.sort_by_key(|response| response.m_u32ResponseCanId);

        // An empty vector is not "no target": the identifier was known and the ECUs did
        // process the request — they were simply all required to stay quiet.
        RoutingOutcome::Handled(vecResponses)
    }
}

/// Drive one ECU with a request and package its answer for the caller.
fn ProcessOnEcu(
    runningEcu: &mut VirtualEcu,
    u32RequestCanId: u32,
    vecRequest: &[u8],
    protocol: &dyn ProtocolHandler,
) -> RoutedResponse {
    // Only ECUs that carry a CAN address are ever started, so reading it back here cannot
    // fail: `BuildEcuMap` skips the rest.
    let address = runningEcu
        .Config()
        .m_optCanAddress
        .expect("a started ECU always has a CAN address; BuildEcuMap only starts those");
    let u32ResponseCanId = address.m_u32ResponseCanId;
    let u32EcuRequestCanId = address.m_u32RequestCanId;
    let strEcuName = runningEcu.Config().m_strName.clone();

    let plan = runningEcu.ProcessRequestWithTiming(protocol, vecRequest);
    let vecResponse = plan.FinalResponse().to_vec();

    if !plan.m_bIsIsoConformant {
        for strWarning in &plan.m_vecConformanceWarnings {
            tracing::warn!(
                ecu = %strEcuName,
                requestCanId = format!("{u32RequestCanId:03X}"),
                "response timing is not ISO 14229-2 conformant: {strWarning}"
            );
        }
    }

    if vecResponse.is_empty() {
        tracing::info!(
            ecu = %strEcuName,
            requestCanId = format!("{u32RequestCanId:03X}"),
            dropped = plan.m_bIsFinalResponseDropped,
            "nothing will be transmitted for this request"
        );
    } else {
        tracing::info!(
            ecu = %strEcuName,
            requestCanId = format!("{u32RequestCanId:03X}"),
            responseCanId = format!("{u32ResponseCanId:03X}"),
            responseBytes = vecResponse.len(),
            responsePending = plan.m_u8ResponsePendingCount,
            finalAtMs = plan.FinalAtMs(),
            "request answered"
        );
    }

    RoutedResponse {
        m_strEcuName: strEcuName,
        m_u32RequestCanId: u32EcuRequestCanId,
        m_u32ResponseCanId: u32ResponseCanId,
        m_vecResponse: vecResponse,
        m_bySession: runningEcu.CurrentSession(),
        m_bIsSecurityUnlocked: runningEcu.IsSecurityUnlocked(),
        m_plan: plan,
    }
}

/// Build the request-identifier -> running-ECU map, rejecting models that cannot be routed.
///
/// An ECU without CAN addressing is skipped rather than failing the whole load: a model can
/// legitimately hold ECUs from a source that carries no CAN addressing, and a partial vehicle
/// is more useful than none (README §7). Only a model with nothing routable at all is refused.
fn BuildEcuMap(vehicle: &Vehicle) -> Result<BTreeMap<u32, VirtualEcu>, SimulationError> {
    if vehicle.m_vecEcus.is_empty() {
        return Err(SimulationError::NoEcusFound);
    }

    let mut mapEcus: BTreeMap<u32, VirtualEcu> = BTreeMap::new();
    for config in &vehicle.m_vecEcus {
        let address = match config.m_optCanAddress {
            Some(address) => address,
            None => {
                tracing::warn!(
                    ecu = %config.m_strName,
                    "ECU has no CAN address and cannot be reached on CAN; it is loaded but not started"
                );
                continue;
            }
        };

        RejectDuplicateIdentifiers(&mapEcus, config, &address)?;
        mapEcus.insert(address.m_u32RequestCanId, VirtualEcu::New(config.clone()));
    }

    if mapEcus.is_empty() {
        return Err(SimulationError::NoRoutableEcus);
    }

    Ok(mapEcus)
}

/// Refuse an ECU whose identifiers collide with one already started: two ECUs on one request
/// identifier make routing ambiguous, and two on one response identifier make their answers
/// indistinguishable on the wire.
fn RejectDuplicateIdentifiers(
    mapEcus: &BTreeMap<u32, VirtualEcu>,
    config: &Ecu,
    address: &CanAddress,
) -> Result<(), SimulationError> {
    if let Some(existing) = mapEcus.get(&address.m_u32RequestCanId) {
        return Err(SimulationError::DuplicateRequestCanId {
            u32RequestCanId: address.m_u32RequestCanId,
            strFirstEcu: existing.Config().m_strName.clone(),
            strSecondEcu: config.m_strName.clone(),
        });
    }

    for existing in mapEcus.values() {
        let optExistingAddress = existing.Config().m_optCanAddress;
        let bSharesResponseId = matches!(
            optExistingAddress,
            Some(existingAddress)
                if existingAddress.m_u32ResponseCanId == address.m_u32ResponseCanId
        );
        if bSharesResponseId {
            return Err(SimulationError::DuplicateResponseCanId {
                u32ResponseCanId: address.m_u32ResponseCanId,
                strFirstEcu: existing.Config().m_strName.clone(),
                strSecondEcu: config.m_strName.clone(),
            });
        }
    }

    Ok(())
}

/// Build the broadcast-identifier -> listening-ECU map from the started ECUs.
///
/// The targets are ordered by response identifier so a broadcast produces answers in the order
/// CAN arbitration would.
fn BuildFunctionalTargetMap(
    mapEcus: &BTreeMap<u32, VirtualEcu>,
) -> Result<BTreeMap<u32, Vec<u32>>, SimulationError> {
    let mut mapTargets: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();

    for (u32RequestCanId, runningEcu) in mapEcus {
        let address = match runningEcu.Config().m_optCanAddress {
            Some(address) => address,
            None => continue,
        };
        let u32FunctionalCanId = match address.m_optU32FunctionalCanId {
            Some(u32FunctionalCanId) => u32FunctionalCanId,
            None => continue,
        };

        // A broadcast identifier that is also some ECU's own request identifier would make
        // routing ambiguous — the physical branch would always win and shadow the broadcast.
        if let Some(shadowed) = mapEcus.get(&u32FunctionalCanId) {
            return Err(SimulationError::FunctionalIdCollidesWithPhysical {
                u32CanId: u32FunctionalCanId,
                strEcuName: shadowed.Config().m_strName.clone(),
            });
        }

        mapTargets
            .entry(u32FunctionalCanId)
            .or_default()
            .push((address.m_u32ResponseCanId, *u32RequestCanId));
    }

    let mut mapOrdered: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (u32FunctionalCanId, mut vecTargets) in mapTargets {
        vecTargets.sort_by_key(|(u32ResponseCanId, _)| *u32ResponseCanId);
        let vecRequestIds = vecTargets
            .into_iter()
            .map(|(_, u32RequestCanId)| u32RequestCanId)
            .collect();
        mapOrdered.insert(u32FunctionalCanId, vecRequestIds);
    }

    Ok(mapOrdered)
}

/// True when this ECU configuration is addressed on the given request identifier.
fn IsAddressedOn(config: &Ecu, u32RequestCanId: u32) -> bool {
    matches!(
        config.m_optCanAddress,
        Some(address) if address.m_u32RequestCanId == u32RequestCanId
    )
}

/// Mirror a timing change into the loaded model, so the model JSON matches what is running.
fn UpdateVehicleTiming(optVehicle: Option<&mut Vehicle>, u32RequestCanId: u32, timing: EcuTiming) {
    let vehicle = match optVehicle {
        Some(vehicle) => vehicle,
        None => return,
    };

    for config in &mut vehicle.m_vecEcus {
        let bIsTarget = matches!(
            config.m_optCanAddress,
            Some(address) if address.m_u32RequestCanId == u32RequestCanId
        );
        if bIsTarget {
            config.m_timing = timing;
            return;
        }
    }
}

/// The NRC of a negative response, or `None` if the bytes are not one.
fn NegativeResponseCodeOf(vecResponse: &[u8]) -> Option<u8> {
    if vecResponse.len() < 3 || vecResponse[0] != c_byNegativeResponseSid {
        return None;
    }
    Some(vecResponse[2])
}

/// True when a negative response must not be sent because the request was functionally
/// addressed (ISO 14229-1 clause 7.5.3.3 Table 5, clause 7.5.4.3 Table 7).
///
/// The rule keeps a broadcast from drawing a chorus of "I do not support that" from every ECU
/// on the bus. It applies to negative responses only; the suppressPosRspMsgIndicationBit is a
/// separate mechanism and does not affect them.
fn IsNegativeResponseSuppressedFunctionally(vecResponse: &[u8]) -> bool {
    match NegativeResponseCodeOf(vecResponse) {
        Some(byNrc) => c_arrFunctionallySuppressedNrcs.contains(&byNrc),
        None => false,
    }
}
