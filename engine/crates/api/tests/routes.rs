//! Route-shape tests: every endpoint the UI calls must actually be reachable.
//!
//! These exist because a route can compile perfectly and still never match — axum 0.7 spells
//! a path parameter `:name`, and `{name}` is matched as a literal segment, so the handler is
//! simply never reached. That failure is invisible to the compiler and to every unit test of
//! the handler itself; only driving the router catches it.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use api::{build_router, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// An engine with no plugins and nothing loaded — enough to prove a route is wired up.
fn EmptyState() -> Arc<AppState> {
    Arc::new(AppState {
        plugins: Vec::new(),
        protocol: None,
        ecu: Mutex::new(ecu::VirtualEcu::New(ecu::sample::BuildEngineEcu())),
        simulation: Mutex::new(simulation::SimulationService::New()),
        busy_ecus: Mutex::new(BTreeSet::new()),
    })
}

async fn StatusOf(strMethod: &str, strPath: &str, strBody: &str) -> StatusCode {
    let request = Request::builder()
        .method(strMethod)
        .uri(strPath)
        .header("content-type", "application/json")
        .body(Body::from(strBody.to_string()))
        .expect("a valid request");

    build_router(EmptyState())
        .oneshot(request)
        .await
        .expect("the router should answer")
        .status()
}

#[tokio::test]
async fn every_simulation_route_is_reachable() {
    // Nothing is loaded, so most of these answer 4xx — but never 404, which is what an
    // unreachable route looks like from the outside.
    let vecRoutes = [
        ("GET", "/health", ""),
        ("GET", "/plugins", ""),
        ("GET", "/simulation/state", ""),
        ("GET", "/hw/ports", ""),
        ("GET", "/simulation/topology", ""),
        ("POST", "/simulation/reset", "{}"),
        (
            "POST",
            "/simulation/load",
            r#"{"logText":"(0.001) can0 7E0#0210030000000000"}"#,
        ),
        (
            "POST",
            "/simulation/request",
            r#"{"canIdHex":"7E0","requestHex":"22F190"}"#,
        ),
        ("POST", "/simulation/vehicle", r#"{"name":"Bench"}"#),
        (
            "POST",
            "/simulation/ecus",
            r#"{"name":"Engine","requestCanIdHex":"7E0","responseCanIdHex":"7E8"}"#,
        ),
    ];

    for (strMethod, strPath, strBody) in vecRoutes {
        let status = StatusOf(strMethod, strPath, strBody).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{strMethod} {strPath} is not reachable"
        );
    }
}

#[tokio::test]
async fn the_per_ecu_timing_route_matches_a_can_identifier() {
    // The regression this file exists for: written as `{requestCanIdHex}` instead of
    // `:requestCanIdHex`, this path matched nothing and the engine answered 404 to every call.
    //
    // A 404 does not prove anything here — with nothing loaded the ECU is genuinely absent, so
    // a working route answers 404 too. The distinguishing case is a *malformed* identifier: it
    // is rejected by the handler with 400, which can only happen if the handler ran.
    let strTimingBody = r#"{"p2ServerMaxMs":50,"p2StarServerMaxMs":5000,"p4ServerMaxMs":30000,
                            "responseDelayMs":0,"forceResponsePending":false,
                            "forcedResponsePendingCount":1,"dropFinalResponse":false}"#;

    for strMethod in ["GET", "PUT"] {
        let status = StatusOf(strMethod, "/simulation/ecus/ZZZ/timing", strTimingBody).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{strMethod} on the timing route returned {status}; it should have reached the handler and been rejected as a bad CAN id"
        );
    }

    // And with a well-formed identifier the handler reports the ECU as absent, which is the
    // right answer when no vehicle is loaded.
    for strMethod in ["GET", "PUT"] {
        assert_eq!(
            StatusOf(strMethod, "/simulation/ecus/7E0/timing", strTimingBody).await,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn the_per_ecu_builder_routes_match_a_can_identifier() {
    // Same trap as the timing route: a path parameter written `{name}` would make these
    // unreachable, and a 404 for a missing ECU looks identical to a 404 for a missing route.
    // A malformed identifier separates them — it can only be rejected by a handler that ran.
    assert_eq!(
        StatusOf("DELETE", "/simulation/ecus/ZZZ", "").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        StatusOf("PUT", "/simulation/ecus/ZZZ/name", r#"{"name":"Engine"}"#).await,
        StatusCode::BAD_REQUEST
    );
}
