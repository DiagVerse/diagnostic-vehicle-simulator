//! Runtime plugin host.
//!
//! Scans a directory for platform dynamic libraries, loads each through the stable-ABI
//! [`plugin_contract`], and keeps the ones that pass abi_stable's layout/version checks.
//! Failures are logged and skipped so one bad library never takes down the engine.

use std::path::{Path, PathBuf};

use abi_stable::library::{LibraryError, RootModule};
use plugin_contract::{PluginKind, PluginModRef};
use serde::Serialize;

/// Serializable, native-Rust view of a successfully loaded plugin. Safe to hand to the API
/// layer (nothing `abi_stable` leaks out of the application layer).
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    /// Plugin-declared unique name.
    pub name: String,
    /// Capability kind (stringified for transport/display).
    pub kind: String,
    /// Plugin's own version.
    pub version: String,
    /// Human-readable description returned by the plugin.
    pub description: String,
    /// Absolute path the library was loaded from.
    pub path: String,
}

/// Holds every plugin that loaded successfully.
///
/// The loaded module references are `'static` (abi_stable leaks them for the process
/// lifetime), so `PluginHost` can be cloned cheaply where needed via its `infos`.
pub struct PluginHost {
    modules: Vec<PluginModRef>,
    infos: Vec<PluginInfo>,
}

impl PluginHost {
    /// Load every compatible plugin found in `dir`. A missing directory yields an empty host
    /// (logged) rather than an error, so a fresh checkout still boots.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut host = PluginHost {
            modules: Vec::new(),
            infos: Vec::new(),
        };

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), %err, "plugin directory not readable; starting with no plugins");
                return host;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !is_dynamic_library(&path) {
                continue;
            }
            match host.try_load(&path) {
                Ok(name) => tracing::info!(plugin = %name, path = %path.display(), "loaded plugin"),
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "skipping incompatible or invalid plugin")
                }
            }
        }

        tracing::info!(count = host.infos.len(), "plugin loading complete");
        host
    }

    /// Attempt to load a single library. abi_stable validates the ABI layout and the
    /// `plugin-contract` version inside `load_from_file`, returning an error on mismatch.
    fn try_load(&mut self, path: &Path) -> Result<String, LibraryError> {
        let module = PluginModRef::load_from_file(path)?;

        let manifest = module.manifest()();
        let description = module.describe()();

        let info = PluginInfo {
            name: manifest.name.to_string(),
            kind: kind_str(manifest.kind).to_string(),
            version: manifest.version.to_string(),
            description: description.to_string(),
            path: path.display().to_string(),
        };
        let name = info.name.clone();

        self.modules.push(module);
        self.infos.push(info);
        Ok(name)
    }

    /// Metadata for all loaded plugins.
    pub fn infos(&self) -> &[PluginInfo] {
        &self.infos
    }

    /// Number of loaded plugins.
    pub fn len(&self) -> usize {
        self.infos.len()
    }

    /// Whether no plugins are loaded.
    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }
}

/// The conventional runtime plugin drop-in directory, relative to the current working dir.
pub fn default_plugin_dir() -> PathBuf {
    PathBuf::from("plugins.d")
}

/// True if the path has a platform dynamic-library extension.
fn is_dynamic_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("so") | Some("dll") | Some("dylib")
    )
}

fn kind_str(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::Sample => "sample",
        PluginKind::Protocol => "protocol",
        PluginKind::Populator => "populator",
        PluginKind::Adapter => "adapter",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_yields_empty_host() {
        let host = PluginHost::load_from_dir(Path::new("/nonexistent/plugins/dir"));
        assert!(host.is_empty());
        assert_eq!(host.len(), 0);
    }

    #[test]
    fn recognizes_dynamic_library_extensions() {
        assert!(is_dynamic_library(Path::new("libfoo.so")));
        assert!(is_dynamic_library(Path::new("foo.dll")));
        assert!(is_dynamic_library(Path::new("libfoo.dylib")));
        assert!(!is_dynamic_library(Path::new("README.md")));
        assert!(!is_dynamic_library(Path::new("foo")));
    }
}
