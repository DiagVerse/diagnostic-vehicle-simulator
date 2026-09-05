//! Plugin contract — the stable ABI boundary between the engine and its plugins.
//!
//! Rust has no stable ABI, so dynamically loaded plugins cannot exchange native Rust types
//! safely across compiler versions. We use [`abi_stable`] to define an FFI-safe, versioned
//! contract: only the types below cross the boundary. `abi_stable` embeds a layout
//! fingerprint and the `plugin-contract` crate version into every plugin, and the host
//! verifies both at load time — incompatible plugins are rejected rather than crashing.
//!
//! Phase 0 exposes a deliberately tiny surface (`manifest` + `describe`) that proves the
//! whole discover → version-check → register → invoke path. Real capability ports
//! (PhysicalLink, Transport, AppProtocol, Populator, …) are layered on in later phases as
//! additional fields on the root module or as separate `#[sabi_trait]` objects.

use abi_stable::{
    declare_root_module_statics, library::RootModule, package_version_strings,
    sabi_types::VersionStrings, std_types::RString, StableAbi,
};

/// Kind of capability a plugin provides. Kept as a string-ish enum so the host can group
/// and display plugins; extended as new port kinds are added.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PluginKind {
    /// A sample/no-op plugin used to exercise the loading machinery (Phase 0).
    Sample,
    /// An OSI protocol-layer implementation (Phase 1+).
    Protocol,
    /// A model populator/parser (Phase 2+).
    Populator,
    /// An infrastructure adapter: persistence, ssh, serial, … (Phase 5+).
    Adapter,
}

/// Self-describing metadata every plugin returns. Cheap to call; used for logging and the
/// `/health` listing.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct PluginManifest {
    /// Unique, human-readable plugin name (e.g. `"uds"`, `"canlog"`).
    pub name: RString,
    /// What the plugin provides.
    pub kind: PluginKind,
    /// Plugin's own semantic version (independent of the contract ABI version).
    pub version: RString,
}

/// The root module exported by every plugin. This is a `prefix type`: new fields may be
/// appended over time and older hosts still load newer plugins (missing newer fields simply
/// aren't called). See `abi_stable`'s prefix-type documentation.
#[repr(C)]
#[derive(StableAbi)]
#[sabi(kind(Prefix(prefix_ref = PluginModRef)))]
#[sabi(missing_field(panic))]
pub struct PluginMod {
    /// Return this plugin's manifest.
    pub manifest: extern "C" fn() -> PluginManifest,

    /// Return a short human-readable description of what the plugin does. Phase 0 uses this
    /// as a smoke-test capability to prove a loaded plugin's code actually runs.
    pub describe: extern "C" fn() -> RString,
}

impl RootModule for PluginModRef {
    // Wires up the per-library statics abi_stable needs.
    declare_root_module_statics! {PluginModRef}

    /// Base file name abi_stable uses when loading from a directory by convention. We load
    /// plugins explicitly by path, but this is still required by the trait.
    const BASE_NAME: &'static str = "dvsim_plugin";
    /// Human-readable module name used in error messages.
    const NAME: &'static str = "dvsim_plugin";
    /// Version of *this contract crate*, embedded into every plugin and checked on load.
    const VERSION_STRINGS: VersionStrings = package_version_strings!();
}
