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

use std::sync::Arc;

use ::simulation::{RoutedResponse, RoutingOutcome, SimulationService};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use core_domain::model::{CanAddress, CanAddressingMode, Ecu, EcuTiming, SessionType};
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
