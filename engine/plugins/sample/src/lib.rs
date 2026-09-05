//! Sample plugin — the smallest thing that implements the plugin contract.
//!
//! Its only job in Phase 0 is to prove that a `cdylib` dropped into `plugins.d/` is
//! discovered, ABI/version-checked, loaded, and invoked by the host. Real plugins replace
//! `describe` with actual capability ports.

use abi_stable::{
    export_root_module, prefix_type::PrefixTypeTrait, sabi_extern_fn, std_types::RString,
};
use plugin_contract::protocol::NoProtocolHandler;
use plugin_contract::{PluginKind, PluginManifest, PluginMod, PluginModRef};

/// Entry point abi_stable calls to build this plugin's root module. The `#[export_root_module]`
/// attribute exports the required unmangled symbol so the host can find it by convention.
#[export_root_module]
fn instantiate_root_module() -> PluginModRef {
    // The sample plugin provides no protocol handler.
    PluginMod {
        manifest,
        describe,
        handle_request: NoProtocolHandler,
    }
    .leak_into_prefix()
}

#[sabi_extern_fn]
fn manifest() -> PluginManifest {
    PluginManifest {
        name: RString::from("sample"),
        kind: PluginKind::Sample,
        version: RString::from(env!("CARGO_PKG_VERSION")),
    }
}

#[sabi_extern_fn]
fn describe() -> RString {
    RString::from("sample plugin: proves the dynamic plugin-loading path works")
}
