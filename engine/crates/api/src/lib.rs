//! API layer — the only bridge between the engine and the outside world (the browser UI or
//! an external client). It is deliberately thin: it serializes application-layer data and
//! exposes it over HTTP. It contains no business logic.

pub mod diagnostics;
pub mod doip;
pub mod hardware;
pub mod simulation;
pub mod traffic;

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use ::simulation::SimulationService;
use application::{PluginInfo, ProtocolPlugin};
use axum::{
    extract::{DefaultBodyLimit, State},
    routing::{delete, get, post, put},
    Json, Router,
};
use ecu::VirtualEcu;
use serde::Serialize;
use tower_http::cors::CorsLayer;

/// Shared state handed to every request.
pub struct AppState {
    /// Metadata for all plugins loaded at startup.
    pub plugins: Vec<PluginInfo>,
    /// The resolved UDS protocol handler, if the plugin was loaded. `None` means the
    /// diagnostics endpoints report the protocol as unavailable rather than failing hard.
    pub protocol: Option<ProtocolPlugin>,
    /// The single demo ECU the diagnostics endpoints drive. Guarded by a mutex because it
    /// holds live, mutable diagnostic state; the lock is only held for synchronous work.
    pub ecu: Mutex<VirtualEcu>,
    /// The loaded vehicle simulation the `/simulation/*` endpoints drive. Guarded by a mutex
    /// for the same reason as `ecu`: it holds live ECU state and is mutated by requests.
    /// Shared rather than owned: the hardware bridge runs in its own task and drives the same
    /// simulation the HTTP endpoints do, so an ECU behaves identically either way.
    pub simulation: Arc<Mutex<SimulationService>>,
    /// Request CAN identifiers whose ECU is part-way through a ResponsePending sequence. Such
    /// an ECU has told the tester it cannot receive another request (ISO 14229-1 Annex A.1),
    /// so a second one is refused instead of mutating its state mid-answer.
    pub busy_ecus: Mutex<BTreeSet<u32>>,
    /// The CAN bridge, when one is running.
    pub hardware: Mutex<hardware::HardwareState>,
    /// The live traffic feed every monitor subscribes to. Publishing to it is cheap and does
    /// nothing when nobody is watching, so the engine never has to ask whether it should.
    pub traffic: traffic::TrafficChannel,
    /// The DoIP entity, when the simulation is on an Ethernet wire.
    pub doip: Mutex<doip::DoIpState>,
}

/// Response body for `GET /health`.
#[derive(Serialize)]
struct Health {
    status: &'static str,
    engine_version: &'static str,
    plugin_count: usize,
}

/// Build the HTTP router. CORS is permissive so the Vite dev server (a different origin) can
/// talk to the engine during development; tighten before any real deployment.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/plugins", get(plugins))
        .route("/ecu/state", get(diagnostics::GetEcuState))
        .route("/ecu/request", post(diagnostics::PostEcuRequest))
        .route("/ecu/reset", post(diagnostics::PostEcuReset))
        .route("/simulation/load", post(simulation::PostSimulationLoad))
        .route(
            "/simulation/simfile",
            post(simulation::PostSimulationSimFile),
        )
        // A capture gets its own, much larger limit: it is binary and legitimately tens of
        // megabytes, while every other body here is text measured in kilobytes. One global
        // limit big enough for a capture would let a JSON route accept one too.
        .route(
            "/simulation/pcap",
            post(simulation::PostSimulationCapture)
                .layer(DefaultBodyLimit::max(simulation::c_uMaxCaptureBodyBytes)),
        )
        .route(
            "/simulation/identity",
            get(simulation::GetVehicleIdentity).put(simulation::PutVehicleIdentity),
        )
        .route("/simulation/state", get(simulation::GetSimulationState))
        .route(
            "/simulation/request",
            post(simulation::PostSimulationRequest),
        )
        .route("/simulation/reset", post(simulation::PostSimulationReset))
        .route("/simulation/start", post(simulation::PostSimulationStart))
        .route("/simulation/stop", post(simulation::PostSimulationStop))
        .route("/hw/ports", get(hardware::GetSerialPorts))
        .route("/hw/status", get(hardware::GetHardwareStatus))
        .route("/hw/start", post(hardware::PostHardwareStart))
        .route("/hw/stop", post(hardware::PostHardwareStop))
        .route("/events", get(traffic::GetEvents))
        .route("/doip/status", get(doip::GetDoIpStatus))
        .route(
            "/doip/settings",
            get(doip::GetDoIpSettings).put(doip::PutDoIpSettings),
        )
        .route("/doip/start", post(doip::PostDoIpStart))
        .route("/doip/stop", post(doip::PostDoIpStop))
        .route("/simulation/topology", get(simulation::GetTopology))
        .route("/simulation/networks", post(simulation::PostDeclareNetwork))
        .route(
            "/simulation/networks/:networkId",
            delete(simulation::DeleteNetwork),
        )
        .route(
            "/simulation/ecus/:requestCanIdHex/placement",
            put(simulation::PutEcuPlacement),
        )
        .route(
            "/simulation/ecus/:requestCanIdHex/enabled",
            put(simulation::PutEcuEnabled),
        )
        .route("/simulation/vehicle", post(simulation::PostCreateVehicle))
        .route("/simulation/ecus", post(simulation::PostAddEcu))
        .route(
            "/simulation/ecus/:requestCanIdHex",
            delete(simulation::DeleteEcu),
        )
        .route(
            "/simulation/ecus/:requestCanIdHex/name",
            put(simulation::PutEcuName),
        )
        // axum 0.7 path parameters are `:name`; `{name}` would be matched as a literal
        // segment, so the route would silently never match.
        .route(
            "/simulation/ecus/:requestCanIdHex/overrides",
            get(simulation::GetEcuOverrides).put(simulation::PutEcuOverrides),
        )
        .route(
            "/simulation/ecus/:requestCanIdHex/timing",
            get(simulation::GetEcuTiming).put(simulation::PutEcuTiming),
        )
        // axum caps a request body at 2 MB unless told otherwise, and it rejects an oversized
        // one *before* any handler runs. That made the per-endpoint size guards unreachable:
        // a caller sending a 3 MB CAN log got a bare 413 and, through a dev proxy, an EPIPE
        // while the body was still being written — which looks like a crash rather than a
        // limit. This is the backstop; the endpoints' own guards are deliberately smaller, so
        // the message a user sees is the one that explains itself.
        .layer(DefaultBodyLimit::max(simulation::c_uMaxRequestBodyBytes))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Bind `addr` and serve the API until the process is stopped. Keeps axum entirely inside
/// this crate so the composition root need not depend on the web framework.
pub async fn serve(addr: SocketAddr, state: Arc<AppState>) -> std::io::Result<()> {
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        engine_version: env!("CARGO_PKG_VERSION"),
        plugin_count: state.plugins.len(),
    })
}

async fn plugins(State(state): State<Arc<AppState>>) -> Json<Vec<PluginInfo>> {
    Json(state.plugins.clone())
}
