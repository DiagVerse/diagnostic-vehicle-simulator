//! Protocol-plugin access.
//!
//! Bridges the stable-ABI protocol capability of a loaded plugin to a native trait
//! (`ProtocolHandler`) the engine can call without knowing it is talking to a `cdylib`. This
//! is the seam that lets the ECU runtime stay independent of any specific protocol or of the
//! plugin mechanism itself.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::std_types::RVec;
use plugin_contract::protocol::{REcuSnapshot, RProtocolOutcome};

use crate::plugin_host::PluginHost;

/// A native handle to something that can process a diagnostic request. Implemented by
/// [`ProtocolPlugin`] (a dynamically-loaded plugin), but the ECU only depends on this trait,
/// so an in-process or test handler works identically.
pub trait ProtocolHandler {
    /// Handle one request against an ECU state snapshot, returning the response + changes.
    fn Handle(&self, vecRequest: RVec<u8>, snapshot: REcuSnapshot) -> RProtocolOutcome;

    /// The protocol's name (e.g. "uds").
    fn Name(&self) -> &str;
}

/// A protocol handler backed by a loaded plugin's stable-ABI `handle_request` function. The
/// underlying module lives for the process lifetime (abi_stable leaks it), so the stored
/// function pointer stays valid.
pub struct ProtocolPlugin {
    m_strName: String,
    m_fnHandle: extern "C" fn(RVec<u8>, REcuSnapshot) -> RProtocolOutcome,
}

impl ProtocolHandler for ProtocolPlugin {
    fn Handle(&self, vecRequest: RVec<u8>, snapshot: REcuSnapshot) -> RProtocolOutcome {
        (self.m_fnHandle)(vecRequest, snapshot)
    }

    fn Name(&self) -> &str {
        &self.m_strName
    }
}

impl PluginHost {
    /// Resolve a loaded protocol plugin by name, or `None` if no protocol plugin with that
    /// name is loaded.
    pub fn FindProtocol(&self, strName: &str) -> Option<ProtocolPlugin> {
        for (module, info) in self.modules.iter().zip(self.infos.iter()) {
            if info.kind == "protocol" && info.name == strName {
                return Some(ProtocolPlugin {
                    m_strName: info.name.clone(),
                    m_fnHandle: module.handle_request(),
                });
            }
        }
        None
    }
}
