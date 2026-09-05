//! Application layer — use cases and the runtime plugin host.
//!
//! This layer orchestrates the pure `core-domain` through ports (traits) and owns the
//! machinery that turns dynamically-loaded libraries into usable capabilities. It performs
//! no protocol or transport work itself — those arrive as plugins.

pub mod plugin_host;

pub use plugin_host::{PluginHost, PluginInfo};
