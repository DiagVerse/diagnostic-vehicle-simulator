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

pub mod execute;

use std::collections::BTreeMap;

use application::ProtocolHandler;
use core_domain::model::{CanAddress, Ecu, EcuTiming, Network, ResponseOverride, Vehicle};
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

/// How the simulation refers to one running ECU internally.
///
/// An ECU may be reachable on CAN, over DoIP, or both — and when it is both, it must be **one**
/// `VirtualEcu`, not two. A tester that enters the extended session over DoIP and then reads a
/// data identifier over CAN has to find the ECU still in that session; two instances keyed
/// separately would diverge while looking identical from outside, which is the worst kind of
/// bug to chase.
///
/// So an ECU is stored under exactly one key — its CAN identifier if it has one, its logical
/// address otherwise — and every other way of addressing it is an index pointing here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EcuKey {
    /// Stored under the request identifier a tester addresses it on.
    Can(u32),
    /// Stored under its DoIP logical address, for an ECU with no CAN addressing at all.
    DoIp(u16),
}

impl EcuKey {
    /// The CAN request identifier this ECU is stored under, if it is a CAN-addressed one.
    ///
    /// The HTTP API uses the CAN identifier as its handle for an ECU, so this is what tells it
    /// which ECUs it can talk about. A DoIP-only ECU has no such handle.
    pub fn RequestCanId(self) -> Option<u32> {
        match self {
            EcuKey::Can(u32RequestCanId) => Some(u32RequestCanId),
            EcuKey::DoIp(_) => None,
        }
    }
}

/// Errors from loading or driving a simulation.
#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// The CAN log could not be parsed or reconstructed.
    #[error("failed to reconstruct a vehicle from the log: {0}")]
    Reconstruct(#[from] reconstruct::ReconstructError),

    /// The simulation file could not be read.
    #[error("failed to read the simulation file: {0}")]
    SimFile(#[from] simfile::SimFileError),

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

    /// Two ECUs claim the same DoIP logical address, so a routed request could reach either.
    #[error(
        "DoIP logical address 0x{u16LogicalAddress:04X} is claimed by both '{strFirstEcu}' and '{strSecondEcu}'"
    )]
    DuplicateLogicalAddress {
        /// The contested logical address.
        u16LogicalAddress: u16,
        /// ECU that claimed it first.
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

    /// The change would leave the vehicle's wiring describing something impossible.
    #[error("{0}")]
    Topology(#[from] core_domain::model::TopologyError),

    /// A network was named that the vehicle does not define.
    #[error("no network with id '{strNetworkId}' is defined on this vehicle")]
    NetworkNotFound {
        /// The id that matched nothing.
        strNetworkId: String,
    },

    /// A network still has ECUs on it, or behind it.
    #[error(
        "network '{strNetworkId}' still has {uEcuCount} ECU(s) on it; move or remove them first"
    )]
    NetworkInUse {
        /// The network being removed.
        strNetworkId: String,
        /// How many ECUs still refer to it.
        uEcuCount: usize,
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

    /// No running ECU is addressed by the given handle.
    #[error("no ECU is addressed by {strHandle}")]
    EcuNotFound {
        /// The handle that matched nothing, described the way it was asked for.
        strHandle: String,
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
    /// The simulation is stopped, so every ECU is off the bus.
    ///
    /// Distinct from `NoTarget`: the wire looks the same — silence either way — but the reason
    /// is entirely different, and an operator who has forgotten they pressed stop needs to be
    /// told which one they are looking at.
    Stopped,
    /// No loaded ECU listens on that request identifier. A real ECU is silent on an
    /// identifier it does not own — it does **not** answer with a negative response — so the
    /// simulator stays silent too.
    NoTarget,
    /// An ECU owns that identifier but is switched off, or sits behind a gateway that is.
    ///
    /// Silence on the wire, exactly like `NoTarget` — but an operator who has just flicked a
    /// switch needs to be told that is why, not left to conclude the request was malformed.
    Silenced {
        /// The ECU that would have answered.
        strEcuName: String,
        /// Why it did not, in words worth showing to whoever asked.
        strReason: String,
    },
    /// One or more ECUs handled the request, in ECU order.
    Handled(Vec<RoutedResponse>),
}

/// A running simulation: the loaded vehicle plus one live ECU per CAN request identifier.
pub struct SimulationService {
    /// The model the running ECUs were built from; kept so the API can describe what is loaded.
    m_optVehicle: Option<Vehicle>,
    /// Live ECUs keyed by the CAN identifier a tester addresses them on. A `BTreeMap` keeps
    /// listing order stable and makes routing an O(log n) lookup.
    m_mapEcus: BTreeMap<EcuKey, VirtualEcu>,
    /// Which stored ECU answers a given DoIP logical address.
    ///
    /// An index, not storage: for an ECU that is reachable both ways this points at its
    /// `EcuKey::Can` entry, so both transports drive the same object and the same state.
    m_mapKeyByLogicalAddress: BTreeMap<u16, EcuKey>,
    /// For each functional (broadcast) identifier, the physical request identifiers of the
    /// ECUs that listen on it, in ascending response-identifier order.
    m_mapFunctionalTargets: BTreeMap<u32, Vec<u32>>,
    /// Whether the ECUs are on the bus. Stopping is the simulator's equivalent of pulling
    /// power: nothing answers, but the model and every ECU's diagnostic state are kept, so
    /// starting again resumes exactly where it left off. Clearing state is what a reset is for.
    m_bIsRunning: bool,
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
            m_mapEcus: BTreeMap::new(),
            m_mapKeyByLogicalAddress: BTreeMap::new(),
            m_mapFunctionalTargets: BTreeMap::new(),
            m_bIsRunning: true,
        }
    }

    /// Put the ECUs on the bus.
    pub fn Start(&mut self) {
        if !self.m_bIsRunning {
            tracing::info!(ecus = self.m_mapEcus.len(), "simulation started");
        }
        self.m_bIsRunning = true;
    }

    /// Take the ECUs off the bus, keeping the model and their diagnostic state.
    pub fn Stop(&mut self) {
        if self.m_bIsRunning {
            tracing::info!(
                ecus = self.m_mapEcus.len(),
                "simulation stopped; ECUs keep their state and will resume where they left off"
            );
        }
        self.m_bIsRunning = false;
    }

    /// Whether the ECUs are currently answering.
    pub fn IsRunning(&self) -> bool {
        self.m_bIsRunning
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
        for (key, runningEcu) in &mapEcus {
            tracing::info!(
                ecu = %runningEcu.Config().m_strName,
                addressedOn = %DescribeKey(*key),
                "ECU started"
            );
        }

        let mapKeyByLogicalAddress = BuildLogicalAddressIndex(&mapEcus)?;
        self.m_mapEcus = mapEcus;
        self.m_mapKeyByLogicalAddress = mapKeyByLogicalAddress;
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
        self.m_mapEcus.clear();
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
    pub fn RunningEcus(&self) -> impl Iterator<Item = (EcuKey, &VirtualEcu)> {
        self.m_mapEcus
            .iter()
            .map(|(key, runningEcu)| (*key, runningEcu))
    }

    /// True when the identifier is a functional (broadcast) address some running ECU listens
    /// on, rather than one ECU's own request address.
    pub fn IsFunctionalCanId(&self, u32CanId: u32) -> bool {
        self.m_mapFunctionalTargets.contains_key(&u32CanId)
    }

    /// Look up one running ECU by the identifier it is addressed on.
    pub fn FindEcuByRequestCanId(&self, u32RequestCanId: u32) -> Option<&VirtualEcu> {
        self.m_mapEcus.get(&EcuKey::Can(u32RequestCanId))
    }

    /// Look up one running ECU by whichever handle names it.
    pub fn FindEcu(&self, key: EcuKey) -> Option<&VirtualEcu> {
        self.m_mapEcus.get(&key)
    }

    /// Load a vehicle described in a simulation file.
    ///
    /// The only one of the three sources that can state how the ECUs are wired, so this is the
    /// only one that produces a vehicle with real networks.
    pub fn LoadFromSimFileText(&mut self, strContent: &str) -> Result<&Vehicle, SimulationError> {
        let vehicle = simfile::LoadFromText(strContent)?;
        self.LoadVehicle(vehicle)
    }

    /// Start an empty vehicle to build up by hand.
    ///
    /// Unlike a reconstruction, an empty vehicle is a legitimate starting point: the user is
    /// about to add ECUs one at a time. Any previously loaded vehicle is replaced.
    pub fn CreateEmptyVehicle(&mut self, strName: &str) -> &Vehicle {
        self.m_mapEcus.clear();
        self.m_mapFunctionalTargets.clear();
        self.m_optVehicle = Some(Vehicle {
            m_strName: strName.to_string(),
            m_vecEcus: Vec::new(),
            // A hand-built vehicle has no buses until someone says so, the same way a
            // reconstructed one never does.
            m_vecNetworks: Vec::new(),
            m_identity: Default::default(),
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

        RejectDuplicateIdentifiers(&self.m_mapEcus, &config, &address)?;

        tracing::info!(
            ecu = %config.m_strName,
            requestCanId = format!("{:03X}", address.m_u32RequestCanId),
            responseCanId = format!("{:03X}", address.m_u32ResponseCanId),
            "ECU added"
        );

        self.m_mapEcus.insert(
            EcuKey::Can(address.m_u32RequestCanId),
            VirtualEcu::New(config.clone()),
        );
        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            vehicle.m_vecEcus.push(config);
        }

        self.RebuildFunctionalTargets()
    }

    /// Remove one ECU and stop it.
    pub fn RemoveEcu(&mut self, key: EcuKey) -> Result<(), SimulationError> {
        let removed = self
            .m_mapEcus
            .remove(&key)
            .ok_or_else(|| SimulationError::EcuNotFound {
                strHandle: DescribeKey(key),
            })?;

        tracing::info!(ecu = %removed.Config().m_strName, "ECU removed");

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            vehicle.m_vecEcus.retain(|config| !MatchesKey(config, key));
        }

        self.RebuildFunctionalTargets()
    }

    /// Rename one ECU.
    ///
    /// A reconstructed ECU is named after the identifier it answers on (`ECU_7E8`), which is
    /// accurate but tells a reader nothing. Naming it "Engine" is the single most useful thing
    /// a user can add to a reconstructed model.
    pub fn RenameEcu(&mut self, key: EcuKey, strName: &str) -> Result<(), SimulationError> {
        let runningEcu =
            self.m_mapEcus
                .get_mut(&key)
                .ok_or_else(|| SimulationError::EcuNotFound {
                    strHandle: DescribeKey(key),
                })?;
        runningEcu.SetName(strName);

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            for config in &mut vehicle.m_vecEcus {
                if MatchesKey(config, key) {
                    config.m_strName = strName.to_string();
                }
            }
        }

        Ok(())
    }

    /// Replace one ECU's response overrides.
    ///
    /// Written to both the running ECU and the loaded model, so what is simulated and what
    /// gets serialized cannot drift. The caller validates each override first.
    pub fn SetEcuOverrides(
        &mut self,
        key: EcuKey,
        vecOverrides: Vec<ResponseOverride>,
    ) -> Result<(), SimulationError> {
        let runningEcu =
            self.m_mapEcus
                .get_mut(&key)
                .ok_or_else(|| SimulationError::EcuNotFound {
                    strHandle: DescribeKey(key),
                })?;
        runningEcu.SetResponseOverrides(vecOverrides.clone());

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            for config in &mut vehicle.m_vecEcus {
                if MatchesKey(config, key) {
                    config.m_vecResponseOverrides = vecOverrides;
                    break;
                }
            }
        }

        Ok(())
    }

    /// One ECU's response overrides.
    pub fn EcuOverridesOf(&self, key: EcuKey) -> Result<&[ResponseOverride], SimulationError> {
        self.m_mapEcus
            .get(&key)
            .map(|runningEcu| runningEcu.Config().m_vecResponseOverrides.as_slice())
            .ok_or_else(|| SimulationError::EcuNotFound {
                strHandle: DescribeKey(key),
            })
    }

    /// Recompute which ECUs listen on each broadcast identifier, after the set of running ECUs
    /// has changed.
    fn RebuildFunctionalTargets(&mut self) -> Result<(), SimulationError> {
        self.m_mapFunctionalTargets = BuildFunctionalTargetMap(&self.m_mapEcus)?;
        // Both indexes are derived from the same ECU map, so they are rebuilt together — an
        // ECU added or removed changes what a broadcast reaches *and* what a logical address
        // resolves to, and letting one go stale would be a silent routing bug.
        self.m_mapKeyByLogicalAddress = BuildLogicalAddressIndex(&self.m_mapEcus)?;
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
    pub fn SetEcuTiming(&mut self, key: EcuKey, timing: EcuTiming) -> Result<(), SimulationError> {
        let runningEcu =
            self.m_mapEcus
                .get_mut(&key)
                .ok_or_else(|| SimulationError::EcuNotFound {
                    strHandle: DescribeKey(key),
                })?;
        runningEcu.SetTiming(timing);

        UpdateVehicleTiming(self.m_optVehicle.as_mut(), key, timing);
        Ok(())
    }

    /// One ECU's current timing parameters.
    pub fn EcuTimingOf(&self, key: EcuKey) -> Result<EcuTiming, SimulationError> {
        self.m_mapEcus
            .get(&key)
            .map(|runningEcu| runningEcu.Timing())
            .ok_or_else(|| SimulationError::EcuNotFound {
                strHandle: DescribeKey(key),
            })
    }

    /// Restart every running ECU: back to the default session with security locked. The
    /// loaded model is kept.
    /// A reset returns *diagnostic state* to default, not *configuration*: rebuilding from
    /// the ECU's own config carries operator-set timing across unchanged, which is what an
    /// operator who set up a fault and then reset the session expects.
    pub fn ResetAllEcus(&mut self) {
        for runningEcu in self.m_mapEcus.values_mut() {
            *runningEcu = VirtualEcu::New(runningEcu.Config().clone());
        }
        tracing::info!(
            ecus = self.m_mapEcus.len(),
            "all ECUs reset to default session"
        );
    }

    /// Add a network to the loaded vehicle, or replace one that already has its id.
    ///
    /// Networks are the one thing a CAN capture can never tell us, so this is how a vehicle
    /// reconstructed from a log — or built from scratch — gets an architecture at all.
    pub fn UpsertNetwork(&mut self, network: Network) -> Result<(), SimulationError> {
        let mut vehicle = self.CloneVehicleForEdit()?;

        match vehicle
            .m_vecNetworks
            .iter_mut()
            .find(|existing| existing.m_strId == network.m_strId)
        {
            Some(existing) => *existing = network.clone(),
            None => vehicle.m_vecNetworks.push(network.clone()),
        }

        self.CommitVehicleEdit(vehicle)?;
        tracing::info!(
            network = %network.m_strId,
            kind = ?network.m_kind,
            "network declared"
        );
        Ok(())
    }

    /// Remove a network, refusing while anything still refers to it.
    ///
    /// Removing it out from under its ECUs would silently move them to "nobody said", which
    /// looks the same on screen as an ECU that was never placed.
    pub fn RemoveNetwork(&mut self, strNetworkId: &str) -> Result<(), SimulationError> {
        let mut vehicle = self.CloneVehicleForEdit()?;

        if vehicle.FindNetwork(strNetworkId).is_none() {
            return Err(SimulationError::NetworkNotFound {
                strNetworkId: strNetworkId.to_string(),
            });
        }

        let uEcuCount = vehicle
            .m_vecEcus
            .iter()
            .filter(|ecu| IsOnOrBehindNetwork(ecu, strNetworkId))
            .count();
        if uEcuCount > 0 {
            return Err(SimulationError::NetworkInUse {
                strNetworkId: strNetworkId.to_string(),
                uEcuCount,
            });
        }

        vehicle
            .m_vecNetworks
            .retain(|network| network.m_strId != strNetworkId);
        self.CommitVehicleEdit(vehicle)?;

        tracing::info!(network = %strNetworkId, "network removed");
        Ok(())
    }

    /// Say where one ECU sits and what it gateways onto.
    ///
    /// The same call serves all three sources: an ECU reconstructed from a log, one clicked
    /// together by hand and one read from a file are placed in the architecture identically.
    pub fn SetEcuPlacement(
        &mut self,
        key: EcuKey,
        optStrNetworkId: Option<String>,
        vecGatewayForNetworkIds: Vec<String>,
    ) -> Result<(), SimulationError> {
        let mut vehicle = self.CloneVehicleForEdit()?;

        let config = vehicle
            .m_vecEcus
            .iter_mut()
            .find(|config| MatchesKey(config, key))
            .ok_or_else(|| SimulationError::EcuNotFound {
                strHandle: DescribeKey(key),
            })?;

        config.m_optStrNetworkId = optStrNetworkId.clone();
        config.m_vecGatewayForNetworkIds = vecGatewayForNetworkIds.clone();
        let strEcuName = config.m_strName.clone();

        self.CommitVehicleEdit(vehicle)?;

        // The running ECU carries its own copy of the configuration; leaving it stale would
        // make the diagram and the simulation disagree about the same ECU.
        if let Some(runningEcu) = self.m_mapEcus.get_mut(&key) {
            runningEcu.SetPlacement(optStrNetworkId.clone(), vecGatewayForNetworkIds.clone());
        }

        tracing::info!(
            ecu = %strEcuName,
            network = ?optStrNetworkId,
            gatewayFor = ?vecGatewayForNetworkIds,
            "ECU placement set"
        );
        Ok(())
    }

    /// Switch one ECU on or off.
    ///
    /// Written to both the running ECU and the loaded model, so what is simulated and what
    /// gets serialized cannot drift — the same rule every other per-ECU setting follows.
    pub fn SetEcuEnabled(&mut self, key: EcuKey, bIsEnabled: bool) -> Result<(), SimulationError> {
        let runningEcu =
            self.m_mapEcus
                .get_mut(&key)
                .ok_or_else(|| SimulationError::EcuNotFound {
                    strHandle: DescribeKey(key),
                })?;
        runningEcu.SetEnabled(bIsEnabled);

        if let Some(vehicle) = self.m_optVehicle.as_mut() {
            for config in &mut vehicle.m_vecEcus {
                if MatchesKey(config, key) {
                    config.m_bIsEnabled = bIsEnabled;
                }
            }
        }
        Ok(())
    }

    /// Take a copy of the loaded vehicle to edit, so a rejected change leaves nothing behind.
    fn CloneVehicleForEdit(&self) -> Result<Vehicle, SimulationError> {
        self.m_optVehicle
            .as_ref()
            .cloned()
            .ok_or(SimulationError::NoVehicleLoaded)
    }

    /// Accept an edited vehicle only if its wiring still describes something that could exist.
    fn CommitVehicleEdit(&mut self, mut vehicle: Vehicle) -> Result<(), SimulationError> {
        vehicle.NormalizeEntryPoints();
        vehicle.ValidateTopology()?;
        self.m_optVehicle = Some(vehicle);
        Ok(())
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
        if !self.m_bIsRunning {
            tracing::debug!(
                requestCanId = format!("{u32RequestCanId:03X}"),
                "simulation is stopped; nothing answers"
            );
            return RoutingOutcome::Stopped;
        }

        if self.m_mapEcus.contains_key(&EcuKey::Can(u32RequestCanId)) {
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
        self.ProcessOnKey(EcuKey::Can(u32RequestCanId), vecRequest, protocol)
    }

    /// Drive one stored ECU, whichever transport found it.
    ///
    /// The single place a request meets an ECU. Both `ProcessByCanId` and
    /// `ProcessByLogicalAddress` end here, which is what guarantees the two transports cannot
    /// drift apart in how they gate sessions, apply overrides or honour the on/off switch.
    fn ProcessOnKey(
        &mut self,
        key: EcuKey,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        if let Some(strReason) = self.DescribeWhySilent(key) {
            let strEcuName = self
                .m_mapEcus
                .get(&key)
                .map(|runningEcu| runningEcu.Config().m_strName.clone())
                .unwrap_or_default();

            tracing::info!(
                ecu = %strEcuName,
                addressedOn = %DescribeKey(key),
                reason = %strReason,
                "request reached an ECU that is off the air; staying silent"
            );
            return RoutingOutcome::Silenced {
                strEcuName,
                strReason,
            };
        }

        let runningEcu = match self.m_mapEcus.get_mut(&key) {
            Some(runningEcu) => runningEcu,
            None => return RoutingOutcome::NoTarget,
        };

        // The identifier the answer is *reported* as coming in on. A DoIP-only ECU has no CAN
        // identifier at all; 0 stands for "not addressed on CAN", and only the CAN paths read
        // this field.
        let u32AddressedOn = key.RequestCanId().unwrap_or(0);
        let response = ProcessOnEcu(runningEcu, u32AddressedOn, vecRequest, protocol);
        RoutingOutcome::Handled(vec![response])
    }

    /// Route one request to the ECU that owns a DoIP logical address.
    ///
    /// The sibling of `ProcessByCanId`, and deliberately the *only* difference between the two
    /// transports: how the target is found. Everything past that point — the session gate, the
    /// overrides, the timing plan, whether the ECU is switched off — is the same code and the
    /// same ECU object, so a tester cannot observe a difference in behaviour between reaching
    /// an ECU over DoIP and reaching it over CAN.
    pub fn ProcessByLogicalAddress(
        &mut self,
        u16TargetAddress: u16,
        vecRequest: &[u8],
        protocol: &dyn ProtocolHandler,
    ) -> RoutingOutcome {
        if !self.m_bIsRunning {
            tracing::debug!(
                targetAddress = format!("{u16TargetAddress:04X}"),
                "simulation is stopped; nothing answers"
            );
            return RoutingOutcome::Stopped;
        }

        let key = match self.m_mapKeyByLogicalAddress.get(&u16TargetAddress) {
            Some(key) => *key,
            None => {
                // The DoIP layer turns this into a diagnostic message NACK 0x03, unknown
                // target address (ISO 13400-2 REQ 7.DoIP-071 AL).
                tracing::debug!(
                    targetAddress = format!("{u16TargetAddress:04X}"),
                    "no ECU carries this DoIP logical address"
                );
                return RoutingOutcome::NoTarget;
            }
        };

        self.ProcessOnKey(key, vecRequest, protocol)
    }

    /// True when some running ECU answers to this DoIP logical address.
    pub fn IsKnownLogicalAddress(&self, u16TargetAddress: u16) -> bool {
        self.m_mapKeyByLogicalAddress
            .contains_key(&u16TargetAddress)
    }

    /// Every DoIP logical address the loaded vehicle answers on, in order.
    pub fn LogicalAddresses(&self) -> impl Iterator<Item = u16> + '_ {
        self.m_mapKeyByLogicalAddress.keys().copied()
    }

    /// Say why an ECU would not answer right now, or `None` when it would.
    ///
    /// Two separate reasons, kept apart because they are fixed differently: the ECU is switched
    /// off, or a gateway between it and the tester is. The second is the first thing the
    /// declared architecture actually enforces — an unpowered gateway takes everything behind
    /// it off the air on a real vehicle, and a switch that did not do that would be decorative.
    fn DescribeWhySilent(&self, key: EcuKey) -> Option<String> {
        let runningEcu = self.m_mapEcus.get(&key)?;
        let config = runningEcu.Config();

        if !config.m_bIsEnabled {
            return Some(format!("'{}' is switched off", config.m_strName));
        }

        let vehicle = self.m_optVehicle.as_ref()?;
        let path = vehicle.DiagnosticPathTo(config);
        let strDisabledGateway = path.m_optStrDisabledGatewayName?;

        Some(format!(
            "the gateway '{strDisabledGateway}' is switched off, so nothing behind it can be reached"
        ))
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
            // A broadcast reaches whoever is listening. An ECU that is off, or behind a
            // gateway that is off, simply is not.
            if let Some(strReason) = self.DescribeWhySilent(EcuKey::Can(u32TargetRequestId)) {
                tracing::debug!(
                    requestCanId = format!("{u32FunctionalCanId:03X}"),
                    reason = %strReason,
                    "an ECU was skipped for this broadcast"
                );
                continue;
            }

            let runningEcu = match self.m_mapEcus.get_mut(&EcuKey::Can(u32TargetRequestId)) {
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
    // A DoIP-addressed ECU is started without any CAN identifiers at all, so these are absent
    // rather than unreadable. Zero stands for "not addressed on CAN" — only the CAN transport
    // reads them back, and it never reaches an ECU that has none.
    let optAddress = runningEcu.Config().m_optCanAddress;
    let u32ResponseCanId = optAddress
        .map(|address| address.m_u32ResponseCanId)
        .unwrap_or(0);
    let u32EcuRequestCanId = optAddress
        .map(|address| address.m_u32RequestCanId)
        .unwrap_or(0);
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
fn BuildEcuMap(vehicle: &Vehicle) -> Result<BTreeMap<EcuKey, VirtualEcu>, SimulationError> {
    if vehicle.m_vecEcus.is_empty() {
        return Err(SimulationError::NoEcusFound);
    }

    let mut mapEcus: BTreeMap<EcuKey, VirtualEcu> = BTreeMap::new();

    for config in &vehicle.m_vecEcus {
        // The key is the CAN identifier when there is one, so everything that already
        // addresses ECUs that way keeps working untouched. An ECU with only a DoIP address is
        // stored under that instead — it is a perfectly reachable ECU, just not over CAN.
        let optKey = match (config.m_optCanAddress, config.m_bHasDoIpAddress) {
            (Some(address), _) => {
                RejectDuplicateIdentifiers(&mapEcus, config, &address)?;
                Some(EcuKey::Can(address.m_u32RequestCanId))
            }
            (None, true) => Some(EcuKey::DoIp(config.m_u16LogicalAddress)),
            (None, false) => None,
        };

        let key = match optKey {
            Some(key) => key,
            None => {
                tracing::warn!(
                    ecu = %config.m_strName,
                    "ECU carries neither CAN identifiers nor a DoIP logical address; it is loaded but not started"
                );
                continue;
            }
        };

        if let Some(existing) = mapEcus.get(&key) {
            return Err(SimulationError::DuplicateLogicalAddress {
                u16LogicalAddress: config.m_u16LogicalAddress,
                strFirstEcu: existing.Config().m_strName.clone(),
                strSecondEcu: config.m_strName.clone(),
            });
        }
        mapEcus.insert(key, VirtualEcu::New(config.clone()));
    }

    if mapEcus.is_empty() {
        return Err(SimulationError::NoRoutableEcus);
    }

    Ok(mapEcus)
}

/// Index every ECU that carries a DoIP logical address, whatever it is stored under.
///
/// An ECU reachable both ways is indexed to its `EcuKey::Can` entry, so a request arriving over
/// DoIP and one arriving over CAN drive the same object — and therefore the same session,
/// security state and overrides.
fn BuildLogicalAddressIndex(
    mapEcus: &BTreeMap<EcuKey, VirtualEcu>,
) -> Result<BTreeMap<u16, EcuKey>, SimulationError> {
    let mut mapIndex: BTreeMap<u16, EcuKey> = BTreeMap::new();

    for (key, runningEcu) in mapEcus {
        let config = runningEcu.Config();
        if !config.m_bHasDoIpAddress {
            continue;
        }

        if let Some(existingKey) = mapIndex.get(&config.m_u16LogicalAddress) {
            let strFirstEcu = mapEcus
                .get(existingKey)
                .map(|existing| existing.Config().m_strName.clone())
                .unwrap_or_default();
            return Err(SimulationError::DuplicateLogicalAddress {
                u16LogicalAddress: config.m_u16LogicalAddress,
                strFirstEcu,
                strSecondEcu: config.m_strName.clone(),
            });
        }
        mapIndex.insert(config.m_u16LogicalAddress, *key);
    }

    Ok(mapIndex)
}

/// Refuse an ECU whose identifiers collide with one already started: two ECUs on one request
/// identifier make routing ambiguous, and two on one response identifier make their answers
/// indistinguishable on the wire.
fn RejectDuplicateIdentifiers(
    mapEcus: &BTreeMap<EcuKey, VirtualEcu>,
    config: &Ecu,
    address: &CanAddress,
) -> Result<(), SimulationError> {
    if let Some(existing) = mapEcus.get(&EcuKey::Can(address.m_u32RequestCanId)) {
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
    mapEcus: &BTreeMap<EcuKey, VirtualEcu>,
) -> Result<BTreeMap<u32, Vec<u32>>, SimulationError> {
    let mut mapTargets: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();

    for (key, runningEcu) in mapEcus {
        let u32RequestCanId = match key.RequestCanId() {
            Some(u32RequestCanId) => u32RequestCanId,
            // A DoIP-only ECU has no CAN identifier to be broadcast to.
            None => continue,
        };
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
        if let Some(shadowed) = mapEcus.get(&EcuKey::Can(u32FunctionalCanId)) {
            return Err(SimulationError::FunctionalIdCollidesWithPhysical {
                u32CanId: u32FunctionalCanId,
                strEcuName: shadowed.Config().m_strName.clone(),
            });
        }

        mapTargets
            .entry(u32FunctionalCanId)
            .or_default()
            .push((address.m_u32ResponseCanId, u32RequestCanId));
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
fn MatchesKey(config: &Ecu, key: EcuKey) -> bool {
    match key {
        EcuKey::Can(u32RequestCanId) => matches!(
            config.m_optCanAddress,
            Some(address) if address.m_u32RequestCanId == u32RequestCanId
        ),
        // Only an ECU that actually carries a routable DoIP address answers to one. A CAN-only
        // ECU's logical address is a placeholder, and matching on it would let a DoIP handle
        // reach an ECU that has no DoIP presence at all.
        EcuKey::DoIp(u16LogicalAddress) => {
            config.m_bHasDoIpAddress && config.m_u16LogicalAddress == u16LogicalAddress
        }
    }
}

/// Mirror a timing change into the loaded model, so the model JSON matches what is running.
fn UpdateVehicleTiming(optVehicle: Option<&mut Vehicle>, key: EcuKey, timing: EcuTiming) {
    let vehicle = match optVehicle {
        Some(vehicle) => vehicle,
        None => return,
    };

    for config in &mut vehicle.m_vecEcus {
        if MatchesKey(config, key) {
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

/// True when an ECU sits on this network or forwards onto it.
fn IsOnOrBehindNetwork(ecu: &Ecu, strNetworkId: &str) -> bool {
    if ecu.m_optStrNetworkId.as_deref() == Some(strNetworkId) {
        return true;
    }
    ecu.m_vecGatewayForNetworkIds
        .iter()
        .any(|strBehindId| strBehindId == strNetworkId)
}

/// How an ECU is addressed, for a log line.
fn DescribeKey(key: EcuKey) -> String {
    match key {
        EcuKey::Can(u32RequestCanId) => format!("CAN {u32RequestCanId:03X}"),
        EcuKey::DoIp(u16LogicalAddress) => format!("DoIP 0x{u16LogicalAddress:04X}"),
    }
}
