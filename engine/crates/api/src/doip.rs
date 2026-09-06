//! Putting the simulation on an Ethernet wire.
//!
//! The DoIP counterpart of `hardware.rs`: start and stop the entity, and report whether a
//! tester could reach it.

#![allow(non_snake_case, non_upper_case_globals)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{extract::State, Json};
use doip_server::{DoIpEntity, DoIpServer, ServerHandle};
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

    let arcEntity = Arc::new(Mutex::new(DoIpEntity::New(
        Arc::clone(&state.simulation),
        u16EntityAddress,
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
