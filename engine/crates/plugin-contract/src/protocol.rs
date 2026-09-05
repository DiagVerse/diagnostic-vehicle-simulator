//! Stable-ABI types for **protocol plugins** (the OSI application layer, e.g. UDS).
//!
//! A protocol plugin is modelled as a *pure function*: it receives the incoming request
//! bytes plus an FFI-safe snapshot of the ECU's live diagnostic state, and returns the
//! response bytes plus a list of state changes for the engine to apply. The plugin never
//! holds a reference to native engine state, so ownership stays entirely on the engine side
//! and nothing unsafe crosses the boundary.
//!
//! All types here are `#[repr(C)]` + `StableAbi` so they are safe to pass between the host
//! and a dynamically-loaded `cdylib`. Field naming follows the project convention.

#![allow(non_snake_case, non_upper_case_globals)]

use abi_stable::{sabi_extern_fn, std_types::RVec, StableAbi};

/// A DID and its value, in FFI-safe form (mirror of `core-domain`'s `DataIdentifier`).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct RDataIdentifier {
    /// 16-bit data identifier.
    pub m_u16Id: u16,
    /// Value bytes.
    pub m_vecValue: RVec<u8>,
}

/// A DTC in FFI-safe form (mirror of `core-domain`'s `DiagnosticTroubleCode`).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct RDtc {
    /// 3-byte DTC packed into a u32.
    pub m_u32Code: u32,
    /// DTC status byte.
    pub m_byStatus: u8,
}

/// A security level in FFI-safe form (mirror of `core-domain`'s `SecurityLevel`).
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct RSecurityLevel {
    /// requestSeed sub-function identifying this level.
    pub m_byRequestSeedSubFunction: u8,
    /// Seed returned on requestSeed.
    pub m_vecSeed: RVec<u8>,
    /// Key expected on sendKey.
    pub m_vecExpectedKey: RVec<u8>,
}

/// FFI-safe snapshot of the ECU state a protocol plugin needs to compute a response.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct REcuSnapshot {
    /// Current session as its UDS sub-function byte.
    pub m_byCurrentSession: u8,
    /// Currently unlocked security level (0 = locked).
    pub m_bySecurityUnlockedLevel: u8,
    /// Level for which a seed was most recently issued (0 = none pending).
    pub m_byActiveSeedLevel: u8,
    /// Supported request service ids.
    pub m_vecSupportedServices: RVec<u8>,
    /// Supported session sub-function bytes.
    pub m_vecSupportedSessions: RVec<u8>,
    /// DIDs available for ReadDataByIdentifier.
    pub m_vecDids: RVec<RDataIdentifier>,
    /// Stored DTCs.
    pub m_vecDtcs: RVec<RDtc>,
    /// Security levels.
    pub m_vecSecurityLevels: RVec<RSecurityLevel>,
}

// Kinds of state change a plugin can request. A small tag+value struct is used instead of a
// data-carrying enum to keep the ABI trivially stable and the intent explicit.
/// Set the current session to `m_byValue` (a session sub-function byte).
pub const c_byStateChangeSetSession: u8 = 1;
/// Record that a seed was issued for security level `m_byValue`.
pub const c_byStateChangeSetActiveSeedLevel: u8 = 2;
/// Unlock security level `m_byValue`.
pub const c_byStateChangeUnlockSecurity: u8 = 3;
/// Return the ECU to the default session (`m_byValue` ignored).
pub const c_byStateChangeResetToDefaultSession: u8 = 4;

/// A single mutation for the engine to apply to the ECU's live state after responding.
#[repr(C)]
#[derive(StableAbi, Clone, Copy, Debug)]
pub struct RStateChange {
    /// One of the `c_byStateChange*` constants.
    pub m_byKind: u8,
    /// Kind-specific value (e.g. the session or security level byte).
    pub m_byValue: u8,
}

/// The result of handling one request.
#[repr(C)]
#[derive(StableAbi, Clone, Debug)]
pub struct RProtocolOutcome {
    /// Response bytes to send back. Empty means "suppress positive response".
    pub m_vecResponse: RVec<u8>,
    /// State changes for the engine to apply, in order.
    pub m_vecChanges: RVec<RStateChange>,
}

/// Signature of a protocol plugin's request handler: `(requestBytes, ecuSnapshot) -> outcome`.
pub type ProtocolHandlerFn = extern "C" fn(RVec<u8>, REcuSnapshot) -> RProtocolOutcome;

/// No-op handler for plugins that do not serve diagnostic requests. Returns an empty
/// outcome (no response, no state changes). The host never calls this for non-protocol
/// plugins, but every plugin must supply a handler because the ABI cannot express an
/// optional bare function pointer.
#[sabi_extern_fn]
pub fn NoProtocolHandler(_vecRequest: RVec<u8>, _snapshot: REcuSnapshot) -> RProtocolOutcome {
    RProtocolOutcome {
        m_vecResponse: RVec::new(),
        m_vecChanges: RVec::new(),
    }
}
