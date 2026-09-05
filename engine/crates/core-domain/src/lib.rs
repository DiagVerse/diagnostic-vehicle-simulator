//! Core domain — the pure business logic of the Diagnostic Vehicle Simulator.
//!
//! This crate holds the Unified Vehicle Model and the rules that operate on it. It has
//! **no I/O**, no protocol code, and no dependency on the plugin system, the web API, or
//! the UI. Everything outward (protocols, parsers, transports, persistence) is an adapter
//! that this layer defines ports for elsewhere; see `plugin-contract`.
//!
//! The Unified Vehicle Model lives in [`model`].

pub mod model;

/// Domain-level error type. Grows as the model gains behaviour.
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    /// A value violated a domain invariant.
    #[error("invalid domain state: {0}")]
    Invalid(String),
}

/// Confidence attached to a reconstructed fact (README §7). Present from Phase 0 so later
/// phases can populate it without reshaping the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Confidence {
    /// Known for certain (e.g. from a specification or a confirmed handshake).
    Confirmed,
    /// Directly seen in a trace.
    Observed,
    /// Deduced, not directly seen.
    Inferred,
    /// Not known.
    Unknown,
    /// Sources disagree.
    Conflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_roundtrips_through_json() {
        let json = serde_json::to_string(&Confidence::Observed).unwrap();
        let back: Confidence = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Confidence::Observed);
    }
}
