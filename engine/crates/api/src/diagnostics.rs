//! Diagnostics endpoints — a thin HTTP surface over a running virtual ECU so the UI can show
//! and drive the Phase 1 engine live. All protocol logic stays in the `ecu`/`uds` layers;
//! this module only parses input, calls `VirtualEcu`, and serializes the result.
//!
//! Response DTOs use idiomatic snake_case field names with `serde(rename_all = "camelCase")`
//! so the JSON the browser consumes is clean camelCase.

#![allow(non_snake_case, non_upper_case_globals)]

use std::sync::Arc;

use axum::{extract::State, Json};
use ecu::sample::BuildEngineEcu;
use ecu::VirtualEcu;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Snapshot of the ECU's state for display.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EcuStateDto {
    pub name: String,
    pub logical_address: u16,
    pub session: u8,
    pub session_name: String,
    pub security_unlocked: bool,
    pub security_level: u8,
    pub supported_services: Vec<u8>,
    pub dids: Vec<u16>,
    pub dtc_count: usize,
    pub protocol_loaded: bool,
}

/// Request body for sending a raw UDS message (hex string, spaces optional).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequestBody {
    pub request_hex: String,
}

/// Result of processing one request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestResultDto {
    pub request_hex: String,
    pub response_hex: String,
    pub suppressed: bool,
    pub session: u8,
    pub session_name: String,
    pub security_unlocked: bool,
    pub error: Option<String>,
}

/// GET /ecu/state — current ECU state.
pub async fn GetEcuState(State(state): State<Arc<AppState>>) -> Json<EcuStateDto> {
    let ecu = state.ecu.lock().expect("ECU mutex poisoned");
    Json(BuildStateDto(&ecu, state.protocol.is_some()))
}

/// POST /ecu/reset — recreate the demo ECU (fresh default session, security locked).
pub async fn PostEcuReset(State(state): State<Arc<AppState>>) -> Json<EcuStateDto> {
    let mut ecu = state.ecu.lock().expect("ECU mutex poisoned");
    *ecu = VirtualEcu::New(BuildEngineEcu());
    tracing::info!("demo ECU reset via API");
    Json(BuildStateDto(&ecu, state.protocol.is_some()))
}

/// POST /ecu/request — send a raw UDS request and return the response plus new state.
pub async fn PostEcuRequest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SendRequestBody>,
) -> Json<RequestResultDto> {
    // Parse the hex request first; a bad request should not touch ECU state.
    let vecRequest = match ParseHex(&body.request_hex) {
        Ok(vecRequest) => vecRequest,
        Err(strError) => return Json(ErrorResult(&body.request_hex, strError)),
    };

    // The protocol plugin must be loaded to answer anything.
    let protocol = match &state.protocol {
        Some(protocol) => protocol,
        None => {
            return Json(ErrorResult(
                &body.request_hex,
                "UDS protocol plugin is not loaded (place libuds_plugin.* in plugins.d/)"
                    .to_string(),
            ));
        }
    };

    let mut ecu = state.ecu.lock().expect("ECU mutex poisoned");
    let vecResponse = ecu.ProcessRequest(protocol, &vecRequest);

    Json(RequestResultDto {
        request_hex: FormatHex(&vecRequest),
        response_hex: FormatHex(&vecResponse),
        suppressed: vecResponse.is_empty(),
        session: ecu.CurrentSession(),
        session_name: SessionName(ecu.CurrentSession()),
        security_unlocked: ecu.IsSecurityUnlocked(),
        error: None,
    })
}

/// Build the state DTO from a locked ECU.
fn BuildStateDto(ecu: &VirtualEcu, bProtocolLoaded: bool) -> EcuStateDto {
    let config = ecu.Config();
    EcuStateDto {
        name: config.m_strName.clone(),
        logical_address: config.m_u16LogicalAddress,
        session: ecu.CurrentSession(),
        session_name: SessionName(ecu.CurrentSession()),
        security_unlocked: ecu.IsSecurityUnlocked(),
        security_level: ecu.SecurityUnlockedLevel(),
        supported_services: config.m_vecSupportedServices.clone(),
        dids: config.m_mapDids.keys().copied().collect(),
        dtc_count: config.m_vecDtcs.len(),
        protocol_loaded: bProtocolLoaded,
    }
}

/// Build an error result that leaves ECU state untouched.
fn ErrorResult(strRequestHex: &str, strError: String) -> RequestResultDto {
    RequestResultDto {
        request_hex: strRequestHex.to_string(),
        response_hex: String::new(),
        suppressed: false,
        session: 0,
        session_name: String::new(),
        security_unlocked: false,
        error: Some(strError),
    }
}

/// Human-readable name for a UDS session sub-function byte.
fn SessionName(bySession: u8) -> String {
    match bySession {
        0x01 => "Default".to_string(),
        0x02 => "Programming".to_string(),
        0x03 => "Extended".to_string(),
        0x04 => "SafetySystem".to_string(),
        other => format!("0x{other:02X}"),
    }
}

/// Format bytes as space-separated uppercase hex.
fn FormatHex(vecBytes: &[u8]) -> String {
    vecBytes
        .iter()
        .map(|byByte| format!("{byByte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a hex string (spaces optional, e.g. "22 F1 90" or "22F190") into bytes.
fn ParseHex(strInput: &str) -> Result<Vec<u8>, String> {
    let strClean: String = strInput.chars().filter(|c| !c.is_whitespace()).collect();

    if strClean.is_empty() {
        return Err("empty request".to_string());
    }
    if !strClean.len().is_multiple_of(2) {
        return Err("hex string must have an even number of digits".to_string());
    }

    let mut vecBytes = Vec::with_capacity(strClean.len() / 2);
    let vecChars: Vec<char> = strClean.chars().collect();
    let mut iIndex = 0;
    while iIndex < vecChars.len() {
        let strByte: String = vecChars[iIndex..iIndex + 2].iter().collect();
        match u8::from_str_radix(&strByte, 16) {
            Ok(byByte) => vecBytes.push(byByte),
            Err(_) => return Err(format!("invalid hex byte '{strByte}'")),
        }
        iIndex += 2;
    }

    Ok(vecBytes)
}
