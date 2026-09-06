//! Putting the simulation on an Ethernet wire.
//!
//! The DoIP counterpart of `hardware.rs`: start and stop the entity, and report whether a
//! tester could reach it.

#![allow(non_snake_case, non_upper_case_globals)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{extract::State, Json};
use doip_server::{DoIpEntity, DoIpServer, DoIpSettings, ServerHandle};
use serde::{Deserialize, Serialize};

use crate::simulation::ApiError;
use crate::traffic::{NowMs, TrafficEvent};
use crate::AppState;

/// What is currently listening, if anything.
#[derive(Default)]
pub struct DoIpState {
    pub m_optHandle: Option<ServerHandle>,
    pub m_strBoundAddress: String,
    pub m_u16EntityAddress: u16,
    /// What the entity says about itself, and what it has been told to say instead.
    ///
    /// Lives here rather than inside the entity so it survives stop/start and can be set before
    /// the server is running. Shared with a running entity, so a change takes effect at once —
    /// injecting a fault only by restarting would make it useless for reproducing one
    /// mid-session, which is exactly when a fault matters.
    pub m_arcMtxSettings: Arc<Mutex<DoIpSettings>>,
}

/// Where to listen, and as which entity.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartDoIpBody {
    /// Address to bind, e.g. `0.0.0.0:13400`. Port 0 lets the OS choose.
    pub bind: String,
    /// The logical address this entity answers vehicle identification on. Left out, the lowest
    /// logical address the loaded vehicle has is used — which for a gatewayed vehicle is
    /// normally the gateway, and is the one a tester expects to reach first.
    #[serde(default)]
    pub entity_address_hex: Option<String>,
}

/// Whether the entity is on a wire.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoIpStatusDto {
    pub running: bool,
    /// The address actually bound, which differs from the requested one when port 0 was asked for.
    pub bound_address: Option<String>,
    pub entity_address_hex: Option<String>,
    /// Every logical address the loaded vehicle answers on, so a tester knows what to target.
    pub logical_addresses_hex: Vec<String>,
}

/// GET /doip/status — whether a tester could reach this simulation over Ethernet.
pub async fn GetDoIpStatus(State(state): State<Arc<AppState>>) -> Json<DoIpStatusDto> {
    let doip = state.doip.lock().expect("DoIP mutex poisoned");
    let simulation = state.simulation.lock().expect("simulation mutex poisoned");

    Json(DoIpStatusDto {
        running: doip.m_optHandle.is_some(),
        bound_address: doip
            .m_optHandle
            .is_some()
            .then(|| doip.m_strBoundAddress.clone()),
        entity_address_hex: doip
            .m_optHandle
            .is_some()
            .then(|| format!("{:04X}", doip.m_u16EntityAddress)),
        logical_addresses_hex: simulation
            .LogicalAddresses()
            .map(|u16Address| format!("{u16Address:04X}"))
            .collect(),
    })
}

/// POST /doip/start — listen for DoIP testers.
pub async fn PostDoIpStart(
    State(state): State<Arc<AppState>>,
    Json(body): Json<StartDoIpBody>,
) -> Result<Json<DoIpStatusDto>, ApiError> {
    let protocol = state
        .protocol
        .clone()
        .ok_or_else(|| ApiError::Conflict("no UDS protocol plugin is loaded".to_string()))?;

    let bindAddress: SocketAddr = body
        .bind
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("'{}' is not an address:port", body.bind)))?;

    let u16EntityAddress = ResolveEntityAddress(&state, body.entity_address_hex.as_deref())?;

    {
        let doip = state.doip.lock().expect("DoIP mutex poisoned");
        if doip.m_optHandle.is_some() {
            return Err(ApiError::Conflict(format!(
                "already listening on {}; stop it first",
                doip.m_strBoundAddress
            )));
        }
    }

    let arcSettings = Arc::clone(
        &state
            .doip
            .lock()
            .expect("DoIP mutex poisoned")
            .m_arcMtxSettings,
    );
    let arcEntity = Arc::new(Mutex::new(DoIpEntity::New(
        Arc::clone(&state.simulation),
        u16EntityAddress,
        arcSettings,
    )));

    let handle = DoIpServer::Start(arcEntity, bindAddress, Arc::from(protocol))
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!("could not listen on {bindAddress}: {error}"))
        })?;

    let strBoundAddress = handle.TcpAddress().to_string();

    {
        let mut doip = state.doip.lock().expect("DoIP mutex poisoned");
        doip.m_strBoundAddress = strBoundAddress.clone();
        doip.m_u16EntityAddress = u16EntityAddress;
        doip.m_optHandle = Some(handle);
    }

    tracing::info!(
        address = %strBoundAddress,
        entityAddress = format!("{u16EntityAddress:04X}"),
        "the simulation is reachable over DoIP"
    );
    state.traffic.Publish(TrafficEvent::Lifecycle {
        at_ms: NowMs(),
        what: format!("DoIP entity 0x{u16EntityAddress:04X} listening on {strBoundAddress}"),
    });

    Ok(GetDoIpStatus(State(state)).await)
}

/// POST /doip/stop — stop listening.
pub async fn PostDoIpStop(State(state): State<Arc<AppState>>) -> Json<DoIpStatusDto> {
    {
        let mut doip = state.doip.lock().expect("DoIP mutex poisoned");
        if let Some(handle) = doip.m_optHandle.take() {
            handle.Stop();
            tracing::info!("stopped listening for DoIP testers");
        }
    }

    state.traffic.Publish(TrafficEvent::Lifecycle {
        at_ms: NowMs(),
        what: "DoIP entity stopped".to_string(),
    });
    GetDoIpStatus(State(state)).await
}

/// Work out which logical address this entity answers as.
///
/// Defaulting to the lowest one the vehicle has is not arbitrary: logical addresses are
/// allocated with gateways low (ISO 13400-2 Table 13 puts the VM-defined gateway block first),
/// so it is normally the gateway — which is the entity a tester expects to find.
fn ResolveEntityAddress(state: &Arc<AppState>, optStrHex: Option<&str>) -> Result<u16, ApiError> {
    if let Some(strHex) = optStrHex {
        let strDigits = strHex
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        return u16::from_str_radix(strDigits, 16)
            .map_err(|_| ApiError::BadRequest(format!("'{strHex}' is not a hex logical address")));
    }

    let simulation = state.simulation.lock().expect("simulation mutex poisoned");
    let optU16Lowest = simulation.LogicalAddresses().next();
    optU16Lowest.ok_or_else(|| {
        ApiError::Conflict(
            "no ECU in the loaded vehicle has a DoIP logical address, so there is no entity to be"
                .to_string(),
        )
    })
}

/// The entity's parameters and fault injection, as the UI sees them.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoIpSettingsDto {
    /// `0x00` not ready, `0x01` ready, `0x02` not supported.
    pub power_mode: u8,
    /// `0x00` gateway, `0x01` node.
    pub node_type: u8,
    /// Concurrent TCP data sockets reported, excluding the reserve the standard requires.
    pub max_sockets: u8,
    /// Reported *and* enforced — a tester that reads this and sends a message that size expects
    /// it to be accepted.
    pub max_data_size: u32,
    /// Answer nothing to a vehicle identification request.
    pub suppress_identification_response: bool,
    /// Force this routing activation response code instead of deciding. Null to decide normally.
    pub forced_routing_activation_code: Option<u8>,
    /// Negatively acknowledge every diagnostic message with this code. Null to route normally.
    pub forced_diagnostic_nack: Option<u8>,
    /// Refuse every message with this generic header NACK code. Null to read them normally.
    pub forced_header_nack: Option<u8>,
    /// True when any of the above is making the entity behave as a healthy one would not.
    ///
    /// Reported so the UI can say so: an entity quietly refusing everything because a knob was
    /// left on is a confusing afternoon.
    #[serde(skip_deserializing)]
    pub is_injecting_faults: bool,
}

/// GET /doip/settings — what this entity says about itself.
pub async fn GetDoIpSettings(State(state): State<Arc<AppState>>) -> Json<DoIpSettingsDto> {
    let doip = state.doip.lock().expect("DoIP mutex poisoned");
    let settings = doip
        .m_arcMtxSettings
        .lock()
        .expect("DoIP settings mutex poisoned");
    Json(BuildSettingsDto(&settings))
}

/// PUT /doip/settings — change what it says, including making it misbehave.
pub async fn PutDoIpSettings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DoIpSettingsDto>,
) -> Result<Json<DoIpSettingsDto>, ApiError> {
    ValidateSettings(&body)?;

    let doip = state.doip.lock().expect("DoIP mutex poisoned");
    let mut settings = doip
        .m_arcMtxSettings
        .lock()
        .expect("DoIP settings mutex poisoned");

    settings.m_byPowerMode = body.power_mode;
    settings.m_byNodeType = body.node_type;
    settings.m_byMaxSockets = body.max_sockets;
    settings.m_u32MaxDataSize = body.max_data_size;
    settings.m_bSuppressIdentificationResponse = body.suppress_identification_response;
    settings.m_optByForcedRoutingActivationCode = body.forced_routing_activation_code;
    settings.m_optByForcedDiagnosticNack = body.forced_diagnostic_nack;
    settings.m_optByForcedHeaderNack = body.forced_header_nack;

    if settings.IsInjectingFaults() {
        tracing::warn!(
            routingActivation = ?settings.m_optByForcedRoutingActivationCode,
            diagnosticNack = ?settings.m_optByForcedDiagnosticNack,
            headerNack = ?settings.m_optByForcedHeaderNack,
            suppressIdentification = settings.m_bSuppressIdentificationResponse,
            "the DoIP entity is now injecting faults"
        );
    } else {
        tracing::info!("DoIP entity settings updated; nothing is being injected");
    }

    Ok(Json(BuildSettingsDto(&settings)))
}

fn BuildSettingsDto(settings: &DoIpSettings) -> DoIpSettingsDto {
    DoIpSettingsDto {
        power_mode: settings.m_byPowerMode,
        node_type: settings.m_byNodeType,
        max_sockets: settings.m_byMaxSockets,
        max_data_size: settings.m_u32MaxDataSize,
        suppress_identification_response: settings.m_bSuppressIdentificationResponse,
        forced_routing_activation_code: settings.m_optByForcedRoutingActivationCode,
        forced_diagnostic_nack: settings.m_optByForcedDiagnosticNack,
        forced_header_nack: settings.m_optByForcedHeaderNack,
        is_injecting_faults: settings.IsInjectingFaults(),
    }
}

/// Refuse settings that would put something on the wire no vehicle would ever send.
///
/// Fault injection is for reproducing what a real entity does wrong, not for inventing bytes the
/// standard does not define — a tester tested against those has been tested against nothing.
fn ValidateSettings(body: &DoIpSettingsDto) -> Result<(), ApiError> {
    if body.power_mode > 0x02 {
        return Err(ApiError::BadRequest(format!(
            "diagnostic power mode 0x{:02X} is not defined; use 00 not ready, 01 ready or 02 not supported",
            body.power_mode
        )));
    }
    if body.node_type > 0x01 {
        return Err(ApiError::BadRequest(format!(
            "node type 0x{:02X} is not defined; use 00 for a gateway or 01 for a node",
            body.node_type
        )));
    }
    if body.max_sockets == 0 {
        return Err(ApiError::BadRequest(
            "an entity that reports zero sockets could never be connected to".to_string(),
        ));
    }
    if body.max_data_size < 64 {
        return Err(ApiError::BadRequest(
            "a maximum data size below 64 bytes would refuse even a routing activation".to_string(),
        ));
    }

    if let Some(byCode) = body.forced_routing_activation_code {
        let bIsKnown = matches!(byCode, 0x00..=0x04 | 0x06 | 0x10);
        if !bIsKnown {
            return Err(ApiError::BadRequest(format!(
                "0x{byCode:02X} is not a routing activation response code this entity can send"
            )));
        }
    }
    if let Some(byCode) = body.forced_diagnostic_nack {
        if !(0x02..=0x06).contains(&byCode) {
            return Err(ApiError::BadRequest(format!(
                "0x{byCode:02X} is not a diagnostic message NACK code (02 to 06 are defined)"
            )));
        }
    }
    if let Some(byCode) = body.forced_header_nack {
        if byCode > 0x04 {
            return Err(ApiError::BadRequest(format!(
                "0x{byCode:02X} is not a generic header NACK code (00 to 04 are defined)"
            )));
        }
    }
    Ok(())
}
