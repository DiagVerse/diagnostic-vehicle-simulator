//! API layer — the only bridge between the engine and the outside world (the browser UI or
//! an external client). It is deliberately thin: it serializes application-layer data and
//! exposes it over HTTP. It contains no business logic.

pub mod diagnostics;
pub mod simulation;

use std::{
    collections::BTreeSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use ::simulation::SimulationService;
use application::{PluginInfo, ProtocolPlugin};
use axum::{
    extract::State,
    routing::{get, post},
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
    pub simulation: Mutex<SimulationService>,
    /// Request CAN identifiers whose ECU is part-way through a ResponsePending sequence. Such
    /// an ECU has told the tester it cannot receive another request (ISO 14229-1 Annex A.1),
    /// so a second one is refused instead of mutating its state mid-answer.
    pub busy_ecus: Mutex<BTreeSet<u32>>,
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
        .route("/simulation/state", get(simulation::GetSimulationState))
        .route(
            "/simulation/request",
            post(simulation::PostSimulationRequest),
        )
        .route("/simulation/reset", post(simulation::PostSimulationReset))
        .route(
            "/simulation/ecus/{requestCanIdHex}/timing",
            get(simulation::GetEcuTiming).put(simulation::PutEcuTiming),
        )
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
