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

use ::simulation::{RoutingOutcome, SimulationService};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use core_domain::model::{CanAddress, CanAddressingMode};
use core_domain::Confidence;
use ecu::VirtualEcu;
use serde::{Deserialize, Serialize};

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

    /// The engine is not in a state where the request makes sense (nothing loaded, no plugin).
    fn Conflict(strMessage: String) -> Self {
        ApiError {
            m_status: StatusCode::CONFLICT,
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

/// The outcome of one routed request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationRequestResultDto {
    pub can_id_hex: String,
    pub request_hex: String,
    /// False when no ECU listens on that identifier — nothing was sent, as on a real bus.
    pub routed: bool,
    pub ecu_name: Option<String>,
    pub response_can_id_hex: Option<String>,
    pub response_hex: String,
    /// True when the ECU handled the request but deliberately sent no response.
    pub suppressed: bool,
    pub session: Option<u8>,
    pub session_name: Option<String>,
    pub security_unlocked: Option<bool>,
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

    let mut simulation = state.simulation.lock().expect("simulation mutex poisoned");
    if !simulation.IsLoaded() {
        return Err(ApiError::Conflict(
            "no vehicle is loaded; POST a CAN log to /simulation/load first".to_string(),
        ));
    }

    let outcome = simulation.ProcessByCanId(u32RequestCanId, &vecRequest, protocol);

    let result = match outcome {
        RoutingOutcome::NoTarget => SimulationRequestResultDto {
            can_id_hex: FormatCanId(u32RequestCanId),
            request_hex: FormatHex(&vecRequest),
            routed: false,
            ecu_name: None,
            response_can_id_hex: None,
            response_hex: String::new(),
            suppressed: false,
            session: None,
            session_name: None,
            security_unlocked: None,
        },
        RoutingOutcome::Handled(vecResponses) => {
            // Physical addressing: exactly one ECU owns a request identifier.
            let response = vecResponses
                .into_iter()
                .next()
                .expect("a handled request always carries at least one answer");

            // The ECU that answered is the one addressed, so its post-request state is what
            // the caller wants to see next to the response.
            let optEcu = simulation.FindEcuByRequestCanId(u32RequestCanId);
            let bySession = optEcu.map(|runningEcu| runningEcu.CurrentSession());

            SimulationRequestResultDto {
                can_id_hex: FormatCanId(u32RequestCanId),
                request_hex: FormatHex(&vecRequest),
                routed: true,
                ecu_name: Some(response.m_strEcuName.clone()),
                response_can_id_hex: Some(FormatCanId(response.m_u32ResponseCanId)),
                response_hex: FormatHex(&response.m_vecResponse),
                suppressed: response.IsSuppressed(),
                session: bySession,
                session_name: bySession.map(SessionName),
                security_unlocked: optEcu.map(|runningEcu| runningEcu.IsSecurityUnlocked()),
            }
        }
    };

    Ok(Json(result))
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
