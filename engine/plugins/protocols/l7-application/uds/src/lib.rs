//! UDS protocol plugin — the dynamically-loaded `cdylib` that implements the OSI L7
//! application protocol (ISO 14229) against the stable plugin contract.
//!
//! This file is only the ABI boundary: it exports the root module and marshals the FFI
//! request/snapshot types into the native types that `handler` operates on. All protocol
//! logic lives in [`handler`] so it can be unit-tested with plain Rust types.

#![allow(non_snake_case, non_upper_case_globals)]

pub mod handler;

use abi_stable::{
    export_root_module,
    prefix_type::PrefixTypeTrait,
    sabi_extern_fn,
    std_types::{RString, RVec},
};
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};
use plugin_contract::{PluginKind, PluginManifest, PluginMod, PluginModRef};

/// Root-module entry point discovered by the host loader.
#[export_root_module]
fn instantiate_root_module() -> PluginModRef {
    PluginMod {
        manifest,
        describe,
        handle_request: HandleUdsRequest,
    }
    .leak_into_prefix()
}

#[sabi_extern_fn]
fn manifest() -> PluginManifest {
    PluginManifest {
        name: RString::from("uds"),
        kind: PluginKind::Protocol,
        version: RString::from(env!("CARGO_PKG_VERSION")),
    }
}

#[sabi_extern_fn]
fn describe() -> RString {
    RString::from("UDS (ISO 14229) application-layer protocol — OSI L7")
}

/// FFI entry point: convert the ABI request/snapshot into native form, run the handler, and
/// convert the reply back into ABI form.
#[sabi_extern_fn]
fn HandleUdsRequest(vecRequest: RVec<u8>, snapshot: REcuSnapshot) -> RProtocolOutcome {
    let reply = handler::HandleRequest(vecRequest.as_slice(), &snapshot);

    RProtocolOutcome {
        m_vecResponse: RVec::from(reply.m_vecResponse),
        m_vecChanges: RVec::from(reply.m_vecChanges),
    }
}
