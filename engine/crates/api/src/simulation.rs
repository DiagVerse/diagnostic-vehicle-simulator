//! Simulation endpoints — load a CAN log into the engine and drive the reconstructed ECUs by
//! CAN address.
//!
//! This is the HTTP face of the MVP: `POST /simulation/load` reconstructs a vehicle from log
//! text, `GET /simulation/state` describes what is running, and `POST /simulation/request`
//! sends one UDS request to whichever ECU owns the given CAN identifier. All routing and
//! protocol logic lives in the `simulation`/`ecu`/`uds` layers; this module only parses input,
//! calls the service, and serializes the result.
//!
//! Response DTOs use idiomatic snake_case field names with `serde(rename_all = "camelCase")`
//! so the JSON the browser consumes is clean camelCase.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeSet;
use std::sync::Arc;

use ::simulation::{RoutedResponse, RoutingOutcome, SimulationService};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use core_domain::model::{
    CanAddress, CanAddressingMode, EchoSpan, Ecu, EcuTiming, OverrideAction, ResponseOverride,
    SessionType,
};
use core_domain::Confidence;
use ecu::schedule::ScheduledResponse;
use ecu::VirtualEcu;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep_until, Duration, Instant};

use crate::diagnostics::{FormatHex, ParseHex, SessionName};
use crate::AppState;

/// Longest CAN log accepted in one upload (characters). A log is pasted or uploaded by a
/// human; anything larger is a mistake or an attempt to exhaust the engine's memory.
const c_uMaxLogTextChars: usize = 8 * 1024 * 1024;

/// The single link the topology view can honestly draw today: everything reachable through one
/// tester connection. A real `Network` type in the model is what would let there be more.
const c_strDiagnosticLinkId: &str = "diagnostic-link";

/// An error returned to the caller as a JSON body with a non-2xx status.
pub struct ApiError {
    m_status: StatusCode,
    m_strMessage: String,
}

/// The JSON shape of an error response.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiErrorDto {
    error: String,
}

impl ApiError {
    /// The caller sent something the engine cannot act on.
    fn BadRequest(strMessage: String) -> Self {
        ApiError {
            m_status: StatusCode::BAD_REQUEST,
            m_strMessage: strMessage,
        }
    }

    /// The engine is not in a state where the request makes sense (nothing loaded, no plugin,
    /// an ECU already busy).
    fn Conflict(strMessage: String) -> Self {
        ApiError {
            m_status: StatusCode::CONFLICT,
            m_strMessage: strMessage,
        }
    }

    /// The caller addressed something that does not exist.
    fn NotFound(strMessage: String) -> Self {
        ApiError {
            m_status: StatusCode::NOT_FOUND,
            m_strMessage: strMessage,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(status = %self.m_status, error = %self.m_strMessage, "simulation request rejected");
        (
            self.m_status,
            Json(ApiErrorDto {
                error: self.m_strMessage,
            }),
        )
            .into_response()
    }
}

/// Request body for `POST /simulation/load`: the raw text of a CAN log.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSimulationBody {
    pub log_text: String,
}

/// Request body for `POST /simulation/vehicle`: the name of the vehicle to start building.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVehicleBody {
    pub name: String,
}

/// Request body for `POST /simulation/ecus`: one ECU to add to the loaded vehicle.
///
/// Only the name and the identifier pair are required. The addressing mode follows from the
/// identifier width unless it is stated, and an ECU created without an explicit capability set
/// gets the default one, so it answers plausibly the moment it exists.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEcuBody {
    pub name: String,
    pub request_can_id_hex: String,
    pub response_can_id_hex: String,
    #[serde(default)]
    pub addressing_mode: Option<String>,
    #[serde(default)]
    pub supported_services: Option<Vec<u8>>,
    #[serde(default)]
    pub logical_address: Option<u16>,
}

/// Request body for renaming an ECU.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameEcuBody {
    pub name: String,
}

/// Request body for `POST /simulation/request`: which CAN identifier to address, and the UDS
/// request bytes as hex.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationRequestBody {
    pub can_id_hex: String,
    pub request_hex: String,
}

/// One running ECU as the UI sees it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationEcuDto {
    pub name: String,
    pub logical_address: u16,
    pub request_can_id_hex: String,
    pub response_can_id_hex: String,
    /// The broadcast identifier this ECU also listens on, if any.
    pub functional_can_id_hex: Option<String>,
    /// The ECU's current timing parameters.
    pub timing: EcuTimingDto,
    pub addressing_mode: String,
    pub address_confidence: String,
    pub session: u8,
    pub session_name: String,
    pub security_unlocked: bool,
    pub security_level: u8,
    pub supported_services: Vec<u8>,
    pub dids: Vec<u16>,
    pub dtc_count: usize,
}

/// What is currently loaded and running.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationStateDto {
    pub loaded: bool,
    /// False when the simulation is stopped: the vehicle is still loaded and every ECU keeps
    /// its state, but nothing answers.
    pub running: bool,
    pub vehicle_name: Option<String>,
    pub protocol_loaded: bool,
    pub ecus: Vec<SimulationEcuDto>,
}

/// One message an ECU put on the wire, with both when it was scheduled and when it actually
/// went out — showing the two side by side is the honest way to demonstrate that the delay is
/// real rather than a stored number.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationFrameDto {
    pub at_ms: u32,
    pub actual_ms: u64,
    pub hex: String,
    /// `"responsePending"` for a NRC 0x78, `"final"` for the final response.
    pub kind: String,
}

/// One ECU's answer to a routed request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResponseDto {
    pub ecu_name: String,
    pub request_can_id_hex: String,
    pub response_can_id_hex: String,
    pub response_hex: String,
    /// True when nothing went on the wire: either the positive response was suppressed, or
    /// fault injection withheld it — `finalResponseDropped` says which. The state change, if
    /// any, still happened.
    pub suppressed: bool,
    pub session: u8,
    pub session_name: String,
    pub security_unlocked: bool,
    /// Every message this ECU sent, in order.
    pub frames: Vec<SimulationFrameDto>,
    pub final_at_ms: Option<u32>,
    pub response_pending_count: u8,
    pub final_response_dropped: bool,
    /// False when the schedule knowingly breaks an ISO 14229-2 timing rule; the messages are
    /// still sent, but the engine does not present them as conformant.
    pub iso_conformant: bool,
    pub conformance_warnings: Vec<String>,
}

/// One user-defined answer, as the UI reads and writes it. Bytes travel as hex strings, the
/// same way every other request and response in this API does.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ResponseOverrideDto {
    /// The request bytes to match, e.g. `"22 F1 90"`. A byte written `**` is a wildcard, so
    /// `"22 ** **"` matches a read of any identifier.
    ///
    /// Wildcards live in this string rather than in a parallel mask field on purpose: two
    /// fields that must stay the same length are two fields that can disagree, and editing the
    /// pattern while a stale mask travelled alongside it was a real bug.
    pub request_hex: String,
    /// Treat the pattern as a prefix, for requests with a variable tail.
    #[serde(default)]
    pub match_trailing_bytes: bool,
    /// "substitute" or "suppress".
    pub action: String,
    /// The response bytes, for a substituting override.
    #[serde(default)]
    pub response_hex: Option<String>,
    /// Runs of the request to copy into the response, so a wildcard override still echoes the
    /// identifier it was asked for.
    #[serde(default)]
    pub echo_spans: Vec<EchoSpanDto>,
    #[serde(default = "TrueByDefault")]
    pub enabled: bool,
    #[serde(default)]
    pub respond_even_if_suppressed: bool,
    #[serde(default)]
    pub note: String,
}

/// A run of request bytes copied into the response.
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct EchoSpanDto {
    pub request_offset: usize,
    pub length: usize,
    pub response_offset: usize,
}

/// An override with no `enabled` field is on: the caller just created it.
fn TrueByDefault() -> bool {
    true
}

/// Request body for replacing an ECU's overrides.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOverridesBody {
    pub overrides: Vec<ResponseOverrideDto>,
}

/// An ECU's timing parameters, as the UI reads and writes them.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EcuTimingDto {
    pub p2_server_max_ms: u32,
    pub p2_star_server_max_ms: u32,
    pub p4_server_max_ms: u32,
    pub response_delay_ms: u32,
    pub force_response_pending: bool,
    pub forced_response_pending_count: u8,
    pub drop_final_response: bool,
}

/// The result of a timing update, with the one thing an operator will otherwise wonder about.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcuTimingUpdateDto {
    #[serde(flatten)]
    pub timing: EcuTimingDto,
    /// Always true: ISO 14229-1 carries P2/P2* only in the DiagnosticSessionControl response,
    /// so a tester does not learn new values until it next requests a session.
    pub advertised_at_next_session_control: bool,
}

/// The outcome of one routed request.
///
/// `responses` holds one entry per ECU that answered. A physically addressed request produces
/// at most one; a functionally addressed one produces an entry per listening ECU, in the order
/// CAN arbitration would put them on the bus. It can legitimately be empty even when
/// `routed` is true — every listener was required to stay silent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationRequestResultDto {
    pub can_id_hex: String,
    pub request_hex: String,
    /// How the identifier was interpreted: `"physical"`, `"functional"`, or `"unrouted"`.
    pub addressing: String,
    /// False when no ECU listens on that identifier — nothing was sent, as on a real bus.
    pub routed: bool,
    pub responses: Vec<SimulationResponseDto>,
}

/// POST /simulation/load — reconstruct a vehicle from CAN-log text and start its ECUs.
pub async fn PostSimulationLoad(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoadSimulationBody>,
) -> Result<Json<SimulationStateDto>, ApiError> {
    if body.log_text.trim().is_empty() {
        return Err(ApiError::BadRequest("the log is empty".to_string()));
    }
    if body.log_text.len() > c_uMaxLogTextChars {
        return Err(ApiError::BadRequest(format!(
            "log is {} characters; the limit is {c_uMaxLogTextChars}",
            body.log_text.len()
        )));
    }

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");

    // A failed load leaves any previously loaded simulation running, so a bad upload cannot
    // destroy a working session.
    simulation
        .LoadFromLogText(&body.log_text)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;

    Ok(Json(BuildStateDto(&simulation, state.protocol.is_some())))
}

/// GET /simulation/state — the loaded vehicle and every running ECU.
pub async fn GetSimulationState(State(state): State<Arc<AppState>>) -> Json<SimulationStateDto> {
    let simulation = state.simulation.lock().expect("simulation mutex poisoned");
    Json(BuildStateDto(&simulation, state.protocol.is_some()))
}

/// POST /simulation/start — put the ECUs back on the bus.
pub async fn PostSimulationStart(State(state): State<Arc<AppState>>) -> Json<SimulationStateDto> {
    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation.Start();
    Json(BuildStateDto(&simulation, state.protocol.is_some()))
}

/// POST /simulation/stop — take them off it, keeping the model and their state.
pub async fn PostSimulationStop(State(state): State<Arc<AppState>>) -> Json<SimulationStateDto> {
    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation.Stop();
    Json(BuildStateDto(&simulation, state.protocol.is_some()))
}

/// POST /simulation/reset — return every running ECU to the default session, keeping the
/// loaded model.
pub async fn PostSimulationReset(State(state): State<Arc<AppState>>) -> Json<SimulationStateDto> {
    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation.ResetAllEcus();
    Json(BuildStateDto(&simulation, state.protocol.is_some()))
}

/// POST /simulation/request — send one UDS request to the ECU addressed by a CAN identifier.
///
/// The answer is a *sequence* over time: an ECU that needs longer than P2Server_max sends NRC
/// 0x78 ResponsePending first. The routing decision is made under the lock and returns
/// immediately; the resulting plan is then executed against the clock with the lock released,
/// so a slow ECU does not block the whole simulation.
pub async fn PostSimulationRequest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SimulationRequestBody>,
) -> Result<Json<SimulationRequestResultDto>, ApiError> {
    // Parse both inputs before touching the simulation, so a malformed request cannot leave
    // ECU state half-changed.
    let u32RequestCanId = ParseCanId(&body.can_id_hex).map_err(ApiError::BadRequest)?;
    let vecRequest = ParseHex(&body.request_hex).map_err(ApiError::BadRequest)?;

    let protocol = match &state.protocol {
        Some(protocol) => protocol,
        None => {
            return Err(ApiError::Conflict(
                "UDS protocol plugin is not loaded (place libuds_plugin.* in plugins.d/)"
                    .to_string(),
            ));
        }
    };

    let (bIsFunctional, outcome) = {
        let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
        if !simulation.IsLoaded() {
            return Err(ApiError::Conflict(
                "no vehicle is loaded; POST a CAN log to /simulation/load first".to_string(),
            ));
        }

        let bIsFunctional = simulation.IsFunctionalCanId(u32RequestCanId);
        let outcome = simulation.ProcessByCanId(u32RequestCanId, &vecRequest, protocol);
        (bIsFunctional, outcome)
        // The guard is dropped here, before anything sleeps. The compiler enforces it: a
        // std::sync::MutexGuard is !Send and could not be held across the awaits below.
    };

    let vecResponses = match outcome {
        RoutingOutcome::Stopped => {
            return Ok(Json(SimulationRequestResultDto {
                can_id_hex: FormatCanId(u32RequestCanId),
                request_hex: FormatHex(&vecRequest),
                addressing: "stopped".to_string(),
                routed: false,
                responses: Vec::new(),
            }));
        }
        RoutingOutcome::NoTarget => {
            return Ok(Json(SimulationRequestResultDto {
                can_id_hex: FormatCanId(u32RequestCanId),
                request_hex: FormatHex(&vecRequest),
                addressing: "unrouted".to_string(),
                routed: false,
                responses: Vec::new(),
            }));
        }
        RoutingOutcome::Handled(vecResponses) => vecResponses,
    };

    // An ECU in the middle of a ResponsePending sequence has told the tester it cannot receive
    // another request (ISO 14229-1 Annex A.1), so a second one is refused rather than allowed
    // to mutate its state mid-answer.
    let busyGuard = BusyEcuGuard::Claim(&state, &vecResponses)?;

    let vecResponseDtos = EmitPlans(&vecResponses).await;
    drop(busyGuard);

    Ok(Json(SimulationRequestResultDto {
        can_id_hex: FormatCanId(u32RequestCanId),
        request_hex: FormatHex(&vecRequest),
        addressing: if bIsFunctional {
            "functional".to_string()
        } else {
            "physical".to_string()
        },
        routed: true,
        responses: vecResponseDtos,
    }))
}

/// Execute every ECU's plan against the clock and report what actually went out.
///
/// All offsets are measured from the same instant — the completion of request reception — so
/// the steps of several ECUs answering one broadcast are merged into a single timeline and
/// emitted in the order a real bus would carry them.
async fn EmitPlans(vecResponses: &[RoutedResponse]) -> Vec<SimulationResponseDto> {
    let mut vecDtos: Vec<SimulationResponseDto> =
        vecResponses.iter().map(BuildResponseDto).collect();

    let mut vecTimeline: Vec<(usize, &ScheduledResponse)> = Vec::new();
    for (uResponseIndex, response) in vecResponses.iter().enumerate() {
        for step in &response.m_plan.m_vecSteps {
            vecTimeline.push((uResponseIndex, step));
        }
    }
    vecTimeline.sort_by_key(|(_, step)| step.m_u32AtMs);

    // An absolute deadline per step, so the small per-iteration overhead cannot accumulate
    // across a long sequence.
    let baseline = Instant::now();
    for (uResponseIndex, step) in vecTimeline {
        sleep_until(baseline + Duration::from_millis(step.m_u32AtMs as u64)).await;

        let u64ActualMs = baseline.elapsed().as_millis() as u64;
        let strKind = if step.m_bIsResponsePending {
            "responsePending"
        } else {
            "final"
        };

        tracing::info!(
            ecu = %vecResponses[uResponseIndex].m_strEcuName,
            responseCanId = %vecDtos[uResponseIndex].response_can_id_hex,
            atMs = step.m_u32AtMs,
            actualMs = u64ActualMs,
            kind = strKind,
            "response frame emitted"
        );

        vecDtos[uResponseIndex].frames.push(SimulationFrameDto {
            at_ms: step.m_u32AtMs,
            actual_ms: u64ActualMs,
            hex: FormatHex(&step.m_vecBytes),
            kind: strKind.to_string(),
        });
    }

    vecDtos
}

/// Marks the ECUs answering one request as busy for as long as their plans are running, and
/// releases them however the request ends.
struct BusyEcuGuard {
    m_state: Arc<AppState>,
    m_vecClaimedRequestCanIds: Vec<u32>,
}

impl BusyEcuGuard {
    /// Claim every ECU whose answer spans time. A single immediate response occupies nothing,
    /// which keeps the common case free of contention.
    fn Claim(state: &Arc<AppState>, vecResponses: &[RoutedResponse]) -> Result<Self, ApiError> {
        let vecWanted: Vec<u32> = vecResponses
            .iter()
            .filter(|response| response.m_plan.m_vecSteps.len() > 1)
            .map(|response| response.m_u32RequestCanId)
            .collect();

        let mut setBusy = state.busy_ecus.lock().expect("busy-ECU mutex poisoned");
        for u32RequestCanId in &vecWanted {
            if setBusy.contains(u32RequestCanId) {
                tracing::warn!(
                    requestCanId = format!("{u32RequestCanId:03X}"),
                    "request refused: the ECU is in the middle of a ResponsePending sequence"
                );
                return Err(ApiError::Conflict(format!(
                    "the ECU on CAN id 0x{u32RequestCanId:03X} is completing a ResponsePending sequence and cannot receive another request yet"
                )));
            }
        }
        for u32RequestCanId in &vecWanted {
            setBusy.insert(*u32RequestCanId);
        }

        Ok(BusyEcuGuard {
            m_state: Arc::clone(state),
            m_vecClaimedRequestCanIds: vecWanted,
        })
    }
}

impl Drop for BusyEcuGuard {
    fn drop(&mut self) {
        if self.m_vecClaimedRequestCanIds.is_empty() {
            return;
        }
        let mut setBusy = self
            .m_state
            .busy_ecus
            .lock()
            .expect("busy-ECU mutex poisoned");
        for u32RequestCanId in &self.m_vecClaimedRequestCanIds {
            setBusy.remove(u32RequestCanId);
        }
    }
}

/// POST /simulation/vehicle — start an empty vehicle to build up by hand.
pub async fn PostCreateVehicle(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateVehicleBody>,
) -> Result<Json<SimulationStateDto>, ApiError> {
    let strName = body.name.trim();
    if strName.is_empty() {
        return Err(ApiError::BadRequest("the vehicle needs a name".to_string()));
    }

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation.CreateEmptyVehicle(strName);
    Ok(Json(BuildStateDto(&simulation, state.protocol.is_some())))
}

/// POST /simulation/ecus — add one ECU to the loaded vehicle and start it.
pub async fn PostAddEcu(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateEcuBody>,
) -> Result<Json<SimulationStateDto>, ApiError> {
    let config = BuildEcuFromBody(&body)?;

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation
        .AddEcu(config)
        .map_err(|error| ApiError::Conflict(error.to_string()))?;

    Ok(Json(BuildStateDto(&simulation, state.protocol.is_some())))
}

/// DELETE /simulation/ecus/{requestCanIdHex} — remove one ECU and stop it.
pub async fn DeleteEcu(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
) -> Result<Json<SimulationStateDto>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation
        .RemoveEcu(u32RequestCanId)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    Ok(Json(BuildStateDto(&simulation, state.protocol.is_some())))
}

/// PUT /simulation/ecus/{requestCanIdHex}/name — rename one ECU.
pub async fn PutEcuName(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
    Json(body): Json<RenameEcuBody>,
) -> Result<Json<SimulationStateDto>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;

    let strName = body.name.trim();
    if strName.is_empty() {
        return Err(ApiError::BadRequest("the ECU needs a name".to_string()));
    }

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation
        .RenameEcu(u32RequestCanId, strName)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    Ok(Json(BuildStateDto(&simulation, state.protocol.is_some())))
}

/// A link in the topology view.
///
/// Deliberately **not** called a bus. A tester-side capture has one vantage point — the
/// diagnostic connector — and frames from an ECU behind a gateway arrive there with nothing to
/// distinguish them from local traffic. Appearing in one capture proves the ECUs are reachable
/// through the same tester connection, not that they share a wire.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyLinkDto {
    pub id: String,
    pub label: String,
    /// "CAN", "CAN-FD" or "Ethernet". Only what the model can support.
    pub kind: String,
    /// Broadcast identifiers a tester can address on this link.
    pub functional_can_ids_hex: Vec<String>,
    /// How confidently the membership of this link is known. Never `Observed` for a link
    /// derived from a capture, because a capture cannot observe bus membership.
    pub membership_confidence: String,
}

/// One node hanging off a link.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyNodeDto {
    pub id: String,
    pub label: String,
    /// "ecu" or "tester".
    pub kind: String,
    pub link_id: Option<String>,
    pub request_can_id_hex: Option<String>,
    pub response_can_id_hex: Option<String>,
    pub addressing_mode: Option<String>,
    /// How the identifier pair was established — rendered so an inferred pair is visibly
    /// weaker than an observed one.
    pub address_confidence: Option<String>,
    /// True when the ECU is in the model but cannot be reached on CAN.
    pub is_unreachable: bool,
}

/// The diagram, plus the caveats that belong next to it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyDto {
    pub vehicle_name: Option<String>,
    pub links: Vec<TopologyLinkDto>,
    pub nodes: Vec<TopologyNodeDto>,
    /// What this diagram does not know, in the user's words rather than a footnote nobody
    /// reads. A picture that looks authoritative and is not is worse than no picture.
    pub caveats: Vec<String>,
}

/// GET /simulation/topology — how the loaded vehicle is wired, as far as anything knows.
pub async fn GetTopology(State(state): State<Arc<AppState>>) -> Json<TopologyDto> {
    let simulation = state.simulation.lock().expect("simulation mutex poisoned");
    Json(BuildTopologyDto(&simulation))
}

/// Derive the diagram from the running ECUs.
///
/// The model has no `Network` type yet, so there is exactly one link and it is honest about
/// being a reachability set rather than a bus.
fn BuildTopologyDto(simulation: &SimulationService) -> TopologyDto {
    let vecEcus: Vec<&VirtualEcu> = simulation
        .RunningEcus()
        .map(|(_, runningEcu)| runningEcu)
        .collect();

    if vecEcus.is_empty() {
        return TopologyDto {
            vehicle_name: simulation
                .Vehicle()
                .map(|vehicle| vehicle.m_strName.clone()),
            links: Vec::new(),
            nodes: Vec::new(),
            caveats: Vec::new(),
        };
    }

    let mut setFunctionalIds: BTreeSet<u32> = BTreeSet::new();
    let mut vecNodes = vec![TopologyNodeDto {
        id: "tester".to_string(),
        label: "Tester".to_string(),
        kind: "tester".to_string(),
        link_id: Some(c_strDiagnosticLinkId.to_string()),
        request_can_id_hex: None,
        response_can_id_hex: None,
        addressing_mode: None,
        address_confidence: None,
        is_unreachable: false,
    }];

    for runningEcu in &vecEcus {
        let config = runningEcu.Config();
        let address = config
            .m_optCanAddress
            .expect("a started ECU always has a CAN address");
        if let Some(u32FunctionalCanId) = address.m_optU32FunctionalCanId {
            setFunctionalIds.insert(u32FunctionalCanId);
        }

        vecNodes.push(TopologyNodeDto {
            id: FormatCanId(address.m_u32RequestCanId),
            label: config.m_strName.clone(),
            kind: "ecu".to_string(),
            link_id: Some(c_strDiagnosticLinkId.to_string()),
            request_can_id_hex: Some(FormatCanId(address.m_u32RequestCanId)),
            response_can_id_hex: Some(FormatCanId(address.m_u32ResponseCanId)),
            addressing_mode: Some(AddressingModeName(address.m_addressingMode)),
            address_confidence: Some(ConfidenceName(address.m_confidence)),
            is_unreachable: false,
        });
    }

    let link = TopologyLinkDto {
        id: c_strDiagnosticLinkId.to_string(),
        label: "Diagnostic link, as captured".to_string(),
        kind: "CAN".to_string(),
        functional_can_ids_hex: setFunctionalIds.iter().copied().map(FormatCanId).collect(),
        // A capture cannot observe bus membership, and a hand-built model has not been asked
        // about it, so the strongest honest claim is that these ECUs were reached together.
        membership_confidence: ConfidenceName(Confidence::Inferred),
    };

    TopologyDto {
        vehicle_name: simulation
            .Vehicle()
            .map(|vehicle| vehicle.m_strName.clone()),
        links: vec![link],
        nodes: vecNodes,
        caveats: BuildTopologyCaveats(&vecEcus),
    }
}

/// The things this diagram cannot know, stated plainly.
fn BuildTopologyCaveats(vecEcus: &[&VirtualEcu]) -> Vec<String> {
    let mut vecCaveats = vec![
        "These ECUs were reached through the same tester connection. That does not prove they share a physical bus — an ECU behind a gateway answers on the same connector."
            .to_string(),
    ];

    let bHasMixedAddressing = vecEcus.iter().any(|runningEcu| IsExtended(runningEcu))
        && vecEcus.iter().any(|runningEcu| !IsExtended(runningEcu));
    if bHasMixedAddressing {
        vecCaveats.push(
            "11-bit and 29-bit ECUs are shown together. Identifier width says nothing about bus membership: one CAN segment carries both, and a gateway can present several segments on one connector."
                .to_string(),
        );
    }

    vecCaveats.push(
        "Only ECUs that answered are here. Bus load, termination, error state and silent nodes are not visible from a tester-side capture."
            .to_string(),
    );
    vecCaveats
}

/// True when this ECU uses 29-bit addressing.
fn IsExtended(runningEcu: &VirtualEcu) -> bool {
    runningEcu
        .Config()
        .m_optCanAddress
        .map(|address| address.IsExtendedId())
        .unwrap_or(false)
}

/// GET /simulation/ecus/{requestCanIdHex}/overrides — one ECU's user-defined answers.
pub async fn GetEcuOverrides(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
) -> Result<Json<Vec<ResponseOverrideDto>>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;

    let simulation = state.simulation.lock().expect("simulation mutex poisoned");
    let vecOverrides = simulation
        .EcuOverridesOf(u32RequestCanId)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    Ok(Json(vecOverrides.iter().map(BuildOverrideDto).collect()))
}

/// PUT /simulation/ecus/{requestCanIdHex}/overrides — replace them.
///
/// The whole list is replaced rather than patched, so what the caller sends is exactly what the
/// ECU ends up with — no reconciling against a list the UI may have a stale copy of.
pub async fn PutEcuOverrides(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
    Json(body): Json<SetOverridesBody>,
) -> Result<Json<Vec<ResponseOverrideDto>>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;

    let mut vecOverrides = Vec::with_capacity(body.overrides.len());
    for (uIndex, dto) in body.overrides.iter().enumerate() {
        let overrideRule = BuildOverride(dto)
            .map_err(|strError| ApiError::BadRequest(format!("override {uIndex}: {strError}")))?;
        overrideRule
            .Validate()
            .map_err(|error| ApiError::BadRequest(format!("override {uIndex}: {error}")))?;
        vecOverrides.push(overrideRule);
    }

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation
        .SetEcuOverrides(u32RequestCanId, vecOverrides)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    let vecStored = simulation
        .EcuOverridesOf(u32RequestCanId)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;
    Ok(Json(vecStored.iter().map(BuildOverrideDto).collect()))
}

/// Serialize one override for the UI.
fn BuildOverrideDto(overrideRule: &ResponseOverride) -> ResponseOverrideDto {
    let (strAction, optResponseHex, vecEchoSpans) = match &overrideRule.m_action {
        OverrideAction::Suppress => ("suppress".to_string(), None, Vec::new()),
        OverrideAction::Substitute {
            m_vecResponse,
            m_vecEchoSpans,
        } => (
            "substitute".to_string(),
            Some(FormatHex(m_vecResponse)),
            m_vecEchoSpans
                .iter()
                .map(|span| EchoSpanDto {
                    request_offset: span.m_uRequestOffset,
                    length: span.m_uLength,
                    response_offset: span.m_uResponseOffset,
                })
                .collect(),
        ),
    };

    ResponseOverrideDto {
        request_hex: FormatHexPattern(
            &overrideRule.m_vecRequestPattern,
            &overrideRule.m_vecRequestMask,
        ),
        match_trailing_bytes: overrideRule.m_bMatchTrailingBytes,
        action: strAction,
        response_hex: optResponseHex,
        echo_spans: vecEchoSpans,
        enabled: overrideRule.m_bIsEnabled,
        respond_even_if_suppressed: overrideRule.m_bRespondEvenIfSuppressed,
        note: overrideRule.m_strNote.clone(),
    }
}

/// Read one override from a request body. Shape errors are reported here; whether the override
/// could be a real exchange is decided by the domain type's own validation.
fn BuildOverride(dto: &ResponseOverrideDto) -> Result<ResponseOverride, String> {
    let (vecPattern, vecMask) = ParseHexPattern(&dto.request_hex)?;

    let action = match dto.action.as_str() {
        "suppress" => OverrideAction::Suppress,
        "substitute" => {
            let strResponse = dto
                .response_hex
                .as_deref()
                .ok_or("a substituting override needs responseHex")?;
            OverrideAction::Substitute {
                m_vecResponse: ParseHex(strResponse)?,
                m_vecEchoSpans: dto
                    .echo_spans
                    .iter()
                    .map(|span| EchoSpan {
                        m_uRequestOffset: span.request_offset,
                        m_uLength: span.length,
                        m_uResponseOffset: span.response_offset,
                    })
                    .collect(),
            }
        }
        strOther => {
            return Err(format!(
                "unknown action '{strOther}'; use substitute or suppress"
            ))
        }
    };

    Ok(ResponseOverride {
        m_vecRequestPattern: vecPattern,
        m_vecRequestMask: vecMask,
        m_bMatchTrailingBytes: dto.match_trailing_bytes,
        m_action: action,
        m_bIsEnabled: dto.enabled,
        m_bRespondEvenIfSuppressed: dto.respond_even_if_suppressed,
        m_strNote: dto.note.clone(),
    })
}

/// Turn a create-ECU request into a domain ECU, rejecting anything that could not be routed.
fn BuildEcuFromBody(body: &CreateEcuBody) -> Result<Ecu, ApiError> {
    let strName = body.name.trim();
    if strName.is_empty() {
        return Err(ApiError::BadRequest("the ECU needs a name".to_string()));
    }

    let u32RequestCanId = ParseCanId(&body.request_can_id_hex).map_err(ApiError::BadRequest)?;
    let u32ResponseCanId = ParseCanId(&body.response_can_id_hex).map_err(ApiError::BadRequest)?;

    if u32RequestCanId == u32ResponseCanId {
        return Err(ApiError::BadRequest(
            "an ECU cannot request and respond on the same CAN id".to_string(),
        ));
    }

    let mode = ResolveAddressingMode(body, u32RequestCanId, u32ResponseCanId)?;

    let mut config = Ecu::New(strName, body.logical_address.unwrap_or(0));
    config.m_optCanAddress = Some(CanAddress::NewSpecified(
        u32RequestCanId,
        u32ResponseCanId,
        mode,
    ));
    config.m_vecSupportedServices = body
        .supported_services
        .clone()
        .unwrap_or_else(DefaultSupportedServices);
    config.m_vecSupportedSessions = DefaultSupportedSessions();

    Ok(config)
}

/// Decide the addressing mode: what the caller said, or what the identifiers imply.
fn ResolveAddressingMode(
    body: &CreateEcuBody,
    u32RequestCanId: u32,
    u32ResponseCanId: u32,
) -> Result<CanAddressingMode, ApiError> {
    match body.addressing_mode.as_deref() {
        None => {
            // A 29-bit identifier cannot be an 11-bit address, so the width decides.
            let bIsExtended = u32RequestCanId > 0x7FF || u32ResponseCanId > 0x7FF;
            Ok(if bIsExtended {
                CanAddressingMode::NormalFixed29Bit
            } else {
                CanAddressingMode::Normal11Bit
            })
        }
        Some("Normal11Bit") => Ok(CanAddressingMode::Normal11Bit),
        Some("NormalFixed29Bit") => Ok(CanAddressingMode::NormalFixed29Bit),
        Some(strOther) => Err(ApiError::BadRequest(format!(
            "unknown addressing mode '{strOther}'; use Normal11Bit or NormalFixed29Bit"
        ))),
    }
}

/// GET /simulation/ecus/{requestCanIdHex}/timing — one ECU's timing parameters.
pub async fn GetEcuTiming(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
) -> Result<Json<EcuTimingDto>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;

    let simulation = state.simulation.lock().expect("simulation mutex poisoned");
    let timing = simulation
        .EcuTimingOf(u32RequestCanId)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    Ok(Json(BuildTimingDto(&timing)))
}

/// PUT /simulation/ecus/{requestCanIdHex}/timing — change one ECU's timing parameters.
///
/// Rejected values are reported rather than clamped, so an operator is never left wondering
/// why the simulator is doing something they did not ask for.
pub async fn PutEcuTiming(
    State(state): State<Arc<AppState>>,
    Path(strRequestCanIdHex): Path<String>,
    Json(body): Json<EcuTimingDto>,
) -> Result<Json<EcuTimingUpdateDto>, ApiError> {
    let u32RequestCanId = ParseCanId(&strRequestCanIdHex).map_err(ApiError::BadRequest)?;
    let timing = BuildTiming(&body);

    timing
        .Validate()
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    simulation
        .SetEcuTiming(u32RequestCanId, timing)
        .map_err(|error| ApiError::NotFound(error.to_string()))?;

    Ok(Json(EcuTimingUpdateDto {
        timing: BuildTimingDto(&timing),
        advertised_at_next_session_control: true,
    }))
}

/// Describe one ECU's answer, before its plan has been executed.
fn BuildResponseDto(response: &RoutedResponse) -> SimulationResponseDto {
    SimulationResponseDto {
        ecu_name: response.m_strEcuName.clone(),
        request_can_id_hex: FormatCanId(response.m_u32RequestCanId),
        response_can_id_hex: FormatCanId(response.m_u32ResponseCanId),
        response_hex: FormatHex(&response.m_vecResponse),
        suppressed: response.IsSuppressed(),
        session: response.m_bySession,
        session_name: SessionName(response.m_bySession),
        security_unlocked: response.m_bIsSecurityUnlocked,
        frames: Vec::new(),
        final_at_ms: response.m_plan.FinalAtMs(),
        response_pending_count: response.m_plan.m_u8ResponsePendingCount,
        final_response_dropped: response.m_plan.m_bIsFinalResponseDropped,
        iso_conformant: response.m_plan.m_bIsIsoConformant,
        conformance_warnings: response.m_plan.m_vecConformanceWarnings.clone(),
    }
}

/// Serialize an ECU's timing parameters.
fn BuildTimingDto(timing: &EcuTiming) -> EcuTimingDto {
    EcuTimingDto {
        p2_server_max_ms: timing.m_u32P2ServerMaxMs,
        p2_star_server_max_ms: timing.m_u32P2StarServerMaxMs,
        p4_server_max_ms: timing.m_u32P4ServerMaxMs,
        response_delay_ms: timing.m_u32ResponseDelayMs,
        force_response_pending: timing.m_bForceResponsePending,
        forced_response_pending_count: timing.m_u8ForcedResponsePendingCount,
        drop_final_response: timing.m_bDropFinalResponse,
    }
}

/// Read timing parameters from a request body. Validation happens separately, on the domain
/// type, so the same rules apply however the values arrive.
fn BuildTiming(dto: &EcuTimingDto) -> EcuTiming {
    EcuTiming {
        m_u32P2ServerMaxMs: dto.p2_server_max_ms,
        m_u32P2StarServerMaxMs: dto.p2_star_server_max_ms,
        m_u32P4ServerMaxMs: dto.p4_server_max_ms,
        m_u32ResponseDelayMs: dto.response_delay_ms,
        m_bForceResponsePending: dto.force_response_pending,
        m_u8ForcedResponsePendingCount: dto.forced_response_pending_count,
        m_bDropFinalResponse: dto.drop_final_response,
    }
}

/// Describe the running simulation for the UI.
fn BuildStateDto(simulation: &SimulationService, bProtocolLoaded: bool) -> SimulationStateDto {
    let vecEcus: Vec<SimulationEcuDto> = simulation
        .RunningEcus()
        .map(|(_, runningEcu)| BuildEcuDto(runningEcu))
        .collect();

    SimulationStateDto {
        loaded: simulation.IsLoaded(),
        running: simulation.IsRunning(),
        vehicle_name: simulation
            .Vehicle()
            .map(|vehicle| vehicle.m_strName.clone()),
        protocol_loaded: bProtocolLoaded,
        ecus: vecEcus,
    }
}

/// Describe one running ECU, including how confidently its CAN addressing is known.
fn BuildEcuDto(runningEcu: &VirtualEcu) -> SimulationEcuDto {
    let config = runningEcu.Config();

    // Every running ECU has a CAN address — the simulation service refuses to start one
    // without it — so this is a genuine invariant rather than a hopeful unwrap.
    let address: CanAddress = config
        .m_optCanAddress
        .expect("a running ECU always has a CAN address; the simulation service enforces it");

    SimulationEcuDto {
        name: config.m_strName.clone(),
        logical_address: config.m_u16LogicalAddress,
        request_can_id_hex: FormatCanId(address.m_u32RequestCanId),
        response_can_id_hex: FormatCanId(address.m_u32ResponseCanId),
        functional_can_id_hex: address.m_optU32FunctionalCanId.map(FormatCanId),
        timing: BuildTimingDto(&config.m_timing),
        addressing_mode: AddressingModeName(address.m_addressingMode),
        address_confidence: ConfidenceName(address.m_confidence),
        session: runningEcu.CurrentSession(),
        session_name: SessionName(runningEcu.CurrentSession()),
        security_unlocked: runningEcu.IsSecurityUnlocked(),
        security_level: runningEcu.SecurityUnlockedLevel(),
        supported_services: config.m_vecSupportedServices.clone(),
        dids: config.m_mapDids.keys().copied().collect(),
        dtc_count: config.m_vecDtcs.len(),
    }
}

/// The services a newly created ECU supports unless the caller says otherwise: everything the
/// loaded UDS plugin implements, so the ECU answers plausibly the moment it exists rather than
/// refusing every request with NRC 0x11 serviceNotSupported.
fn DefaultSupportedServices() -> Vec<u8> {
    vec![
        0x10, // DiagnosticSessionControl
        0x11, // ECUReset
        0x19, // ReadDTCInformation
        0x22, // ReadDataByIdentifier
        0x27, // SecurityAccess
        0x31, // RoutineControl
        0x3E, // TesterPresent
    ]
}

/// The sessions a newly created ECU can enter. Default is mandatory (ISO 14229-1: an ECU powers
/// up in it); the other two are what a tester actually asks for.
fn DefaultSupportedSessions() -> Vec<SessionType> {
    vec![
        SessionType::Default,
        SessionType::Extended,
        SessionType::Programming,
    ]
}

/// Name an addressing mode for display.
fn AddressingModeName(mode: CanAddressingMode) -> String {
    match mode {
        CanAddressingMode::Normal11Bit => "Normal11Bit".to_string(),
        CanAddressingMode::NormalFixed29Bit => "NormalFixed29Bit".to_string(),
    }
}

/// Name a confidence state for display.
fn ConfidenceName(confidence: Confidence) -> String {
    match confidence {
        Confidence::Confirmed => "Confirmed".to_string(),
        Confidence::Observed => "Observed".to_string(),
        Confidence::Inferred => "Inferred".to_string(),
        Confidence::Unknown => "Unknown".to_string(),
        Confidence::Conflict => "Conflict".to_string(),
    }
}

/// Format a CAN identifier the way the UI and logs show it: bare uppercase hex, at least three
/// digits for 11-bit identifiers and eight for 29-bit ones.
fn FormatCanId(u32CanId: u32) -> String {
    if u32CanId > 0x7FF {
        format!("{u32CanId:08X}")
    } else {
        format!("{u32CanId:03X}")
    }
}

/// Parse a request pattern: hex bytes where `**` (also `??`, `..` or `xx`) means "any value".
///
/// Returns the pattern bytes alongside the mask they imply, so a caller never has to keep two
/// lists in step.
fn ParseHexPattern(strInput: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut vecPattern = Vec::new();
    let mut vecMask = Vec::new();

    for strToken in SplitPatternTokens(strInput) {
        if IsWildcardToken(&strToken) {
            vecPattern.push(0x00);
            vecMask.push(0x00);
            continue;
        }

        let byValue = u8::from_str_radix(&strToken, 16)
            .map_err(|_| format!("'{strToken}' is not a hex byte or a wildcard (**)"))?;
        vecPattern.push(byValue);
        vecMask.push(0xFF);
    }

    if vecPattern.is_empty() {
        return Err("the request pattern is empty".to_string());
    }
    Ok((vecPattern, vecMask))
}

/// Split a pattern into two-character tokens, accepting both `22 F1 90` and `22F190`.
fn SplitPatternTokens(strInput: &str) -> Vec<String> {
    let strClean: String = strInput.chars().filter(|c| !c.is_whitespace()).collect();
    strClean
        .chars()
        .collect::<Vec<char>>()
        .chunks(2)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// True for the spellings of "any value" a person might reasonably type.
fn IsWildcardToken(strToken: &str) -> bool {
    matches!(strToken, "**" | "??" | ".." | "xx" | "XX")
}

/// Render a pattern and its mask back as one string, with wildcards where the mask allows any
/// value.
fn FormatHexPattern(vecPattern: &[u8], vecMask: &[u8]) -> String {
    vecPattern
        .iter()
        .enumerate()
        .map(|(uIndex, byValue)| {
            let bIsWildcard = vecMask.get(uIndex).copied().unwrap_or(0xFF) == 0x00;
            if bIsWildcard {
                "**".to_string()
            } else {
                format!("{byValue:02X}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a CAN identifier written as hex, with or without a `0x` prefix.
fn ParseCanId(strInput: &str) -> Result<u32, String> {
    let strTrimmed = strInput.trim();
    if strTrimmed.is_empty() {
        return Err("empty CAN id".to_string());
    }

    let strDigits = strTrimmed
        .strip_prefix("0x")
        .or_else(|| strTrimmed.strip_prefix("0X"))
        .unwrap_or(strTrimmed);

    let u32CanId = u32::from_str_radix(strDigits, 16)
        .map_err(|_| format!("'{strInput}' is not a hex CAN id"))?;

    // 29 bits is the widest identifier CAN defines.
    if u32CanId > 0x1FFF_FFFF {
        return Err(format!(
            "CAN id 0x{u32CanId:X} is wider than the 29-bit maximum"
        ));
    }

    Ok(u32CanId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_can_ids_with_and_without_a_prefix() {
        assert_eq!(ParseCanId("7E0"), Ok(0x7E0));
        assert_eq!(ParseCanId("0x7e0"), Ok(0x7E0));
        assert_eq!(ParseCanId(" 18DA10F1 "), Ok(0x18DA10F1));
    }

    #[test]
    fn rejects_malformed_and_oversized_can_ids() {
        assert!(ParseCanId("").is_err());
        assert!(ParseCanId("zz").is_err());
        assert!(ParseCanId("FFFFFFFF").is_err());
    }

    #[test]
    fn a_pattern_carries_its_own_wildcards() {
        // Wildcards live in the pattern string precisely so a caller never has to keep a
        // parallel mask the same length as it — editing one and forgetting the other was a
        // real bug, and changing a sub-function changes the byte count whenever the new
        // sub-function takes different parameters.
        assert_eq!(
            ParseHexPattern("22 ** **"),
            Ok((vec![0x22, 0x00, 0x00], vec![0xFF, 0x00, 0x00]))
        );
        assert_eq!(
            ParseHexPattern("19 02 FF"),
            Ok((vec![0x19, 0x02, 0xFF], vec![0xFF, 0xFF, 0xFF]))
        );
        // Changing the sub-function to one with a different parameter shape just works.
        assert_eq!(
            ParseHexPattern("19 04 12 34 56 01"),
            Ok((vec![0x19, 0x04, 0x12, 0x34, 0x56, 0x01], vec![0xFF; 6]))
        );
    }

    #[test]
    fn a_pattern_round_trips_through_its_string_form() {
        for strPattern in ["19 02 FF", "22 ** **", "2E FD 01 55", "36 **"] {
            let (vecPattern, vecMask) = ParseHexPattern(strPattern).expect("a valid pattern");
            assert_eq!(FormatHexPattern(&vecPattern, &vecMask), strPattern);
        }
    }

    #[test]
    fn a_pattern_accepts_unspaced_hex_and_rejects_nonsense() {
        assert_eq!(
            ParseHexPattern("19 0A"),
            ParseHexPattern("190A"),
            "spacing is presentation, not meaning"
        );
        assert!(ParseHexPattern("").is_err());
        assert!(ParseHexPattern("ZZ 01").is_err());
    }

    #[test]
    fn formats_11_bit_and_29_bit_ids_differently() {
        assert_eq!(FormatCanId(0x7E0), "7E0");
        assert_eq!(FormatCanId(0x18DA10F1), "18DA10F1");
    }
}

#[cfg(test)]
mod emitter_tests {
    use super::*;
    use ecu::schedule::ResponsePlan;

    /// One ECU answering after a ResponsePending: `7F 22 78` at 50 ms, the real answer at
    /// 200 ms.
    fn PendingThenAnswer() -> RoutedResponse {
        RoutedResponse {
            m_strEcuName: "Engine_ECU".to_string(),
            m_u32RequestCanId: 0x7E0,
            m_u32ResponseCanId: 0x7E8,
            m_vecResponse: vec![0x62, 0xF1, 0x90],
            m_bySession: 0x01,
            m_bIsSecurityUnlocked: false,
            m_plan: ResponsePlan {
                m_vecSteps: vec![
                    ScheduledResponse {
                        m_u32AtMs: 50,
                        m_vecBytes: vec![0x7F, 0x22, 0x78],
                        m_bIsResponsePending: true,
                    },
                    ScheduledResponse {
                        m_u32AtMs: 200,
                        m_vecBytes: vec![0x62, 0xF1, 0x90],
                        m_bIsResponsePending: false,
                    },
                ],
                m_bIsFinalResponseDropped: false,
                m_bIsIsoConformant: true,
                m_vecConformanceWarnings: Vec::new(),
                m_u8ResponsePendingCount: 1,
            },
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_emitter_waits_for_each_scheduled_offset() {
        let vecResponses = vec![PendingThenAnswer()];

        let baseline = Instant::now();
        let vecDtos = EmitPlans(&vecResponses).await;
        let u64ElapsedMs = baseline.elapsed().as_millis() as u64;

        assert_eq!(vecDtos.len(), 1);
        let vecFrames = &vecDtos[0].frames;
        assert_eq!(vecFrames.len(), 2);

        assert_eq!(vecFrames[0].kind, "responsePending");
        assert_eq!(vecFrames[0].hex, "7F 22 78");
        assert_eq!(vecFrames[0].at_ms, 50);
        assert_eq!(vecFrames[0].actual_ms, 50);

        assert_eq!(vecFrames[1].kind, "final");
        assert_eq!(vecFrames[1].at_ms, 200);
        assert_eq!(vecFrames[1].actual_ms, 200);

        // The handler does not return before the final response has gone out.
        assert!(u64ElapsedMs >= 200, "elapsed {u64ElapsedMs} ms");
    }

    #[tokio::test(start_paused = true)]
    async fn a_broadcast_merges_every_ecus_schedule_into_one_timeline() {
        // The slower ECU's ResponsePending falls between the other ECU's single answer and
        // its own final response, so the merge order — not each plan in turn — is what the
        // bus would carry.
        let mut fast = PendingThenAnswer();
        fast.m_strEcuName = "Body_ECU".to_string();
        fast.m_u32RequestCanId = 0x745;
        fast.m_u32ResponseCanId = 0x765;
        fast.m_plan = ResponsePlan {
            m_vecSteps: vec![ScheduledResponse {
                m_u32AtMs: 100,
                m_vecBytes: vec![0x7E, 0x00],
                m_bIsResponsePending: false,
            }],
            m_u8ResponsePendingCount: 0,
            m_bIsIsoConformant: true,
            ..ResponsePlan::default()
        };

        let vecResponses = vec![PendingThenAnswer(), fast];
        let vecDtos = EmitPlans(&vecResponses).await;

        assert_eq!(vecDtos[0].frames.len(), 2);
        assert_eq!(vecDtos[1].frames.len(), 1);
        assert_eq!(vecDtos[1].frames[0].actual_ms, 100);
        // Emitted between the first ECU's pending (50 ms) and its answer (200 ms).
        assert_eq!(vecDtos[0].frames[0].actual_ms, 50);
        assert_eq!(vecDtos[0].frames[1].actual_ms, 200);
    }
}
