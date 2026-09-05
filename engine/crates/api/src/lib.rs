//! API layer — the only bridge between the engine and the outside world (the browser UI or
//! an external client). It is deliberately thin: it serializes application-layer data and
//! exposes it over HTTP. It contains no business logic.

use std::{net::SocketAddr, sync::Arc};

use application::PluginInfo;
use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use tower_http::cors::CorsLayer;

/// Shared, read-only state handed to every request.
pub struct AppState {
    /// Metadata for all plugins loaded at startup.
    pub plugins: Vec<PluginInfo>,
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
