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
    /// Key expected on sendKey. Empty unless the policy is to compare against it.
    pub m_vecExpectedKey: RVec<u8>,
    /// What to do with the key a tester sends.
    pub m_keyPolicy: RKeyPolicy,
    /// The code to refuse with under [`RKeyPolicy::RefuseWith`]. Ignored by every other policy.
    ///
    /// A plain byte alongside the discriminant rather than a payload inside it: an ABI boundary
    /// is the wrong place for a data-carrying enum, and the pair survives a plugin built
    /// against an older header far more gracefully.
    pub m_byRefusalNrc: u8,
}

/// FFI-safe mirror of `core-domain`'s `SecurityKeyPolicy`.
#[repr(u8)]
#[derive(StableAbi, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RKeyPolicy {
    /// Compare the key against `m_vecExpectedKey`; unlock on a match, NRC 0x35 otherwise.
    #[default]
    CompareWithExpectedKey,
    /// Accept whatever arrives and unlock.
    AcceptAnyKey,
    /// Never unlock; answer with `m_byRefusalNrc`.
    RefuseWith,
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
    /// The block sequence counter the next TransferData must carry (ISO 14229-1 clause 14.2).
    /// Meaningless unless `m_bIsTransferInProgress`.
    pub m_byExpectedBlockSequenceCounter: u8,
    /// Whether a RequestDownload or RequestUpload is open.
    ///
    /// Separate from the counter rather than folded into it as a zero sentinel: the counter
    /// wraps 0xFF to 0x00, so 0x00 is a perfectly ordinary block number and a transfer that
    /// used it as "idle" would die at the wrap — one block short of 256.
    pub m_bIsTransferInProgress: bool,
    /// True when the vehicle is in permissive mode: the server's own gates — session
    /// restrictions, security locks, services absent from the supported list — are not
    /// enforced, so a response the operator configured is always reachable.
    ///
    /// It does not invent answers. A request with nothing configured still gets the honest
    /// refusal, because claiming success for something nobody stated is the one thing a
    /// simulator must not do.
    pub m_bIsPermissive: bool,
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
/// Open a transfer, or advance it: the value is the counter the next TransferData must carry.
pub const c_byStateChangeSetBlockSequenceCounter: u8 = 5;
/// Close a transfer. The value is ignored.
pub const c_byStateChangeEndTransfer: u8 = 6;

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
