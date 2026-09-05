//! Unified Vehicle Model — Phase 1 subset.
//!
//! This module holds the pure data model an ECU simulation operates on: the ECUs, the
//! diagnostic services/DIDs/DTCs they expose, the sessions they support, and their security
//! configuration. It is **configuration + observed facts only** — the live, mutable runtime
//! state of a running ECU (current session, whether security is unlocked) lives in the `ecu`
//! runtime crate, not here.
//!
//! Naming follows the project convention (see CLAUDE.md), which requires allowing Rust's
//! `non_snake_case` lint for this module. Serialized structs use `serde(rename_all)` so the
//! persisted JSON stays readable.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::Confidence;

/// UDS diagnostic session type — the sub-function of service 0x10 (DiagnosticSessionControl,
/// ISO 14229). A running ECU is always in exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SessionType {
    /// 0x01 — the session an ECU powers up in.
    #[default]
    Default,
    /// 0x02 — used for reprogramming/flashing.
    Programming,
    /// 0x03 — unlocks extended diagnostics.
    Extended,
    /// 0x04 — safety-system diagnostics.
    SafetySystem,
}

impl SessionType {
    /// Map to the UDS sub-function byte used on the wire.
    pub fn ToSubFunction(self) -> u8 {
        match self {
            SessionType::Default => 0x01,
            SessionType::Programming => 0x02,
            SessionType::Extended => 0x03,
            SessionType::SafetySystem => 0x04,
        }
    }

    /// Map a UDS sub-function byte back to a session type. Returns `None` for unknown values
    /// so the caller can answer with the appropriate negative response.
    pub fn FromSubFunction(bySubFunction: u8) -> Option<SessionType> {
        match bySubFunction {
            0x01 => Some(SessionType::Default),
            0x02 => Some(SessionType::Programming),
            0x03 => Some(SessionType::Extended),
            0x04 => Some(SessionType::SafetySystem),
            _ => None,
        }
    }
}

/// A Data Identifier (DID) and the value the ECU returns for it (service 0x22).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataIdentifier {
    /// 16-bit DID, e.g. 0xF190 (VIN).
    pub m_u16Id: u16,
    /// Raw bytes the ECU reports for this DID.
    pub m_vecValue: Vec<u8>,
    /// How the value was established (specified, observed in a trace, inferred, …).
    pub m_confidence: Confidence,
}

/// A stored Diagnostic Trouble Code (reported by service 0x19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticTroubleCode {
    /// 3-byte DTC packed into a u32 (ISO 14229 DTC format), e.g. 0x123456.
    pub m_u32Code: u32,
    /// DTC status byte (ISO 14229-1 status-of-DTC bitfield).
    pub m_byStatus: u8,
    /// How this DTC was established.
    pub m_confidence: Confidence,
}

/// Security-access configuration for a single level (service 0x27).
///
/// Phase 1 uses a deliberately simple fixed seed/key pair per level so behaviour is
/// deterministic and testable. Real seed/key algorithms arrive with ODX/security phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityLevel {
    /// The requestSeed sub-function that identifies this level (odd value, e.g. 0x01).
    pub m_byRequestSeedSubFunction: u8,
    /// The seed the ECU returns when this level's seed is requested.
    pub m_vecSeed: Vec<u8>,
    /// The key the ECU expects back to unlock this level.
    pub m_vecExpectedKey: Vec<u8>,
}

impl SecurityLevel {
    /// The sendKey sub-function paired with this level (requestSeed + 1, per ISO 14229).
    pub fn SendKeySubFunction(&self) -> u8 {
        self.m_byRequestSeedSubFunction.wrapping_add(1)
    }
}

/// ISO 14229-1 Table 29: P2Server_max travels in the DiagnosticSessionControl response as two
/// bytes at 1 ms resolution, so it cannot exceed this.
pub const c_u32P2ServerMaxLimitMs: u32 = 65_535;
/// ISO 14229-1 Table 29: P2*Server_max travels as two bytes at 10 ms resolution.
pub const c_u32P2StarServerMaxLimitMs: u32 = 655_350;
/// The resolution P2*Server_max is advertised at (ISO 14229-1 Table 29).
pub const c_u32P2StarResolutionMs: u32 = 10;

/// P2Server_max recommended by ISO 14229-2 Table 4.
pub const c_u32DefaultP2ServerMaxMs: u32 = 50;
/// P2*Server_max recommended by ISO 14229-2 Table 4.
pub const c_u32DefaultP2StarServerMaxMs: u32 = 5_000;
/// P4Server_max default. ISO 14229-2 Table 4 fixes only the minimum (P4 >= P2) and leaves the
/// maximum to the manufacturer; 30 s is a common enhanced-diagnostics value and leaves room
/// for a realistic ResponsePending sequence.
pub const c_u32DefaultP4ServerMaxMs: u32 = 30_000;

/// Upper bound on the operator-injected response delay. Not an ISO limit — a simulator
/// control must not be able to wedge a diagnostic session for an hour.
pub const c_u32MaxResponseDelayMs: u32 = 60_000;
/// Upper bound on P4Server_max, for the same reason.
pub const c_u32MaxP4ServerMaxMs: u32 = 600_000;
/// Upper bound on forced ResponsePending repetitions. ISO 14229-1 sets no ceiling ("may be
/// repeated"), but a control that can emit hundreds is a footgun rather than a feature.
pub const c_u8MaxForcedResponsePendingCount: u8 = 10;

/// UDS server timing (ISO 14229-2 clause 7), in milliseconds.
///
/// Two kinds of value live here and are kept apart by name: the parameters the ECU
/// **advertises and is judged against** (P2, P2*, P4), and the operator's **fault-injection**
/// knobs (response delay, forced ResponsePending, dropped final response). The first group
/// comes from the standard; the second exists so a demo can show the ResponsePending path
/// without waiting, and can simulate a server that never finishes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EcuTiming {
    /// P2Server_max — the deadline to *start* the response after the request has been
    /// completely received. ISO 14229-2 Table 3; recommended 50 ms (Table 4). Advertised in
    /// the DiagnosticSessionControl response at 1 ms resolution.
    pub m_u32P2ServerMaxMs: u32,

    /// P2*Server_max — the deadline to start the response after a ResponsePending (NRC 0x78)
    /// has finished transmitting. ISO 14229-2 Table 3; recommended 5000 ms. Advertised at
    /// 10 ms resolution, so it must be a whole multiple of 10.
    pub m_u32P2StarServerMaxMs: u32,

    /// P4Server_max — the deadline for the *final* response, measured from request reception.
    /// ISO 14229-2 clause 7.1.1. `P4 == P2` means this server may not use NRC 0x78 at all.
    ///
    /// A named default is required: `#[serde(default)]` would use `u32`'s default of 0, which
    /// would make `P4 < P2` on every model written before this field existed and silently
    /// forbid ResponsePending everywhere.
    #[serde(default = "DefaultP4ServerMaxMs")]
    pub m_u32P4ServerMaxMs: u32,

    /// Operator-injected think-time before the final response, measured from request
    /// reception. 0 answers immediately. A value above P2 automatically produces the
    /// ResponsePending sequence the standard requires. Not an ISO parameter.
    #[serde(default)]
    pub m_u32ResponseDelayMs: u32,

    /// Emit a ResponsePending sequence even when the delay alone would not require one, so
    /// the pending path can be demonstrated without a long wait.
    #[serde(default)]
    pub m_bForceResponsePending: bool,

    /// How many NRC 0x78 messages to emit when forcing. Ignored unless
    /// `m_bForceResponsePending` is set. If the delay demands more, the larger value wins.
    #[serde(default = "DefaultForcedResponsePendingCount")]
    pub m_u8ForcedResponsePendingCount: u8,

    /// Withhold the final response entirely after any pending messages: a hung server, so a
    /// tester's P2*Client timeout handling can be exercised. Fault injection.
    #[serde(default)]
    pub m_bDropFinalResponse: bool,
}

/// Named serde default for P4Server_max — see the field's doc comment.
fn DefaultP4ServerMaxMs() -> u32 {
    c_u32DefaultP4ServerMaxMs
}

/// Named serde default for the forced pending count: forcing zero repetitions is a
/// contradiction, so the type default of 0 would be wrong.
fn DefaultForcedResponsePendingCount() -> u8 {
    1
}

impl Default for EcuTiming {
    fn default() -> Self {
        EcuTiming {
            m_u32P2ServerMaxMs: c_u32DefaultP2ServerMaxMs,
            m_u32P2StarServerMaxMs: c_u32DefaultP2StarServerMaxMs,
            m_u32P4ServerMaxMs: c_u32DefaultP4ServerMaxMs,
            m_u32ResponseDelayMs: 0,
            m_bForceResponsePending: false,
            m_u8ForcedResponsePendingCount: DefaultForcedResponsePendingCount(),
            m_bDropFinalResponse: false,
        }
    }
}

/// Why a proposed set of timing parameters was refused.
///
/// The rule these encode: a value that **cannot be truthfully put on the wire** is rejected,
/// while a *behaviour* that is on the wire but non-conformant (a flooding server, one that
/// never answers) is allowed — that is the point of a fault injector — and is flagged on the
/// resulting response plan instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimingValidationError {
    /// P2Server_max does not fit the two advertised bytes.
    #[error("P2Server_max is {u32ValueMs} ms; it is advertised in two bytes at 1 ms resolution, so the limit is {u32LimitMs} ms")]
    P2ServerMaxTooLarge { u32ValueMs: u32, u32LimitMs: u32 },

    /// P2*Server_max does not fit the two advertised bytes.
    #[error("P2*Server_max is {u32ValueMs} ms; it is advertised in two bytes at 10 ms resolution, so the limit is {u32LimitMs} ms")]
    P2StarServerMaxTooLarge { u32ValueMs: u32, u32LimitMs: u32 },

    /// P2*Server_max is advertised in 10 ms units, so a value between units cannot be stated.
    #[error("P2*Server_max is {u32ValueMs} ms; it is advertised in 10 ms units, so use {u32LowerMs} ms or {u32UpperMs} ms")]
    P2StarServerMaxNotAMultiple {
        u32ValueMs: u32,
        u32LowerMs: u32,
        u32UpperMs: u32,
    },

    /// ISO 14229-2 Table 4: P4Server_max's minimum is P2Server_max.
    #[error(
        "P4Server_max is {u32P4Ms} ms but must be at least P2Server_max, which is {u32P2Ms} ms"
    )]
    P4ServerMaxBelowP2 { u32P4Ms: u32, u32P2Ms: u32 },

    /// P4Server_max beyond the simulator's sanity limit.
    #[error("P4Server_max is {u32ValueMs} ms; the simulator's limit is {u32LimitMs} ms")]
    P4ServerMaxTooLarge { u32ValueMs: u32, u32LimitMs: u32 },

    /// The injected delay is beyond the simulator's sanity limit.
    #[error("response delay is {u32ValueMs} ms; the simulator's limit is {u32LimitMs} ms")]
    ResponseDelayTooLarge { u32ValueMs: u32, u32LimitMs: u32 },

    /// The final response must start by P4Server_max, so a longer delay is unreachable.
    #[error("response delay is {u32DelayMs} ms but the final response must start by P4Server_max, which is {u32P4Ms} ms")]
    ResponseDelayBeyondP4 { u32DelayMs: u32, u32P4Ms: u32 },

    /// Forcing zero repetitions is a contradiction.
    #[error("ResponsePending is forced but the repetition count is 0")]
    ForcedResponsePendingCountIsZero,

    /// Too many forced repetitions.
    #[error("forced ResponsePending count is {u8Value}; the simulator's limit is {u8Limit}")]
    ForcedResponsePendingCountTooLarge { u8Value: u8, u8Limit: u8 },

    /// A ResponsePending with no enhanced budget to spend says nothing.
    #[error("ResponsePending is forced but P2*Server_max is 0, so there is no enhanced timing budget to announce")]
    ForcedResponsePendingWithoutP2Star,

    /// ISO 14229-2 clause 7.1.1: `P4 == P2` means NRC 0x78 is not allowed for this server.
    #[error("ResponsePending is forced but P4Server_max equals P2Server_max ({u32P2Ms} ms), which means NRC 0x78 is not allowed for this server")]
    ForcedResponsePendingWithP4EqualToP2 { u32P2Ms: u32 },
}

impl EcuTiming {
    /// Check that these parameters can be honestly advertised and honoured.
    ///
    /// Rejects values, never behaviours: a delay above P2 is fine (the schedule inserts the
    /// ResponsePending messages the standard requires), and so is a deliberately flooding or
    /// hung server. What is refused is a number that cannot be put on the wire truthfully.
    pub fn Validate(&self) -> Result<(), TimingValidationError> {
        if self.m_u32P2ServerMaxMs > c_u32P2ServerMaxLimitMs {
            return Err(TimingValidationError::P2ServerMaxTooLarge {
                u32ValueMs: self.m_u32P2ServerMaxMs,
                u32LimitMs: c_u32P2ServerMaxLimitMs,
            });
        }

        if self.m_u32P2StarServerMaxMs > c_u32P2StarServerMaxLimitMs {
            return Err(TimingValidationError::P2StarServerMaxTooLarge {
                u32ValueMs: self.m_u32P2StarServerMaxMs,
                u32LimitMs: c_u32P2StarServerMaxLimitMs,
            });
        }

        if !self
            .m_u32P2StarServerMaxMs
            .is_multiple_of(c_u32P2StarResolutionMs)
        {
            let u32LowerMs =
                (self.m_u32P2StarServerMaxMs / c_u32P2StarResolutionMs) * c_u32P2StarResolutionMs;
            return Err(TimingValidationError::P2StarServerMaxNotAMultiple {
                u32ValueMs: self.m_u32P2StarServerMaxMs,
                u32LowerMs,
                u32UpperMs: u32LowerMs + c_u32P2StarResolutionMs,
            });
        }

        if self.m_u32P4ServerMaxMs < self.m_u32P2ServerMaxMs {
            return Err(TimingValidationError::P4ServerMaxBelowP2 {
                u32P4Ms: self.m_u32P4ServerMaxMs,
                u32P2Ms: self.m_u32P2ServerMaxMs,
            });
        }

        if self.m_u32P4ServerMaxMs > c_u32MaxP4ServerMaxMs {
            return Err(TimingValidationError::P4ServerMaxTooLarge {
                u32ValueMs: self.m_u32P4ServerMaxMs,
                u32LimitMs: c_u32MaxP4ServerMaxMs,
            });
        }

        if self.m_u32ResponseDelayMs > c_u32MaxResponseDelayMs {
            return Err(TimingValidationError::ResponseDelayTooLarge {
                u32ValueMs: self.m_u32ResponseDelayMs,
                u32LimitMs: c_u32MaxResponseDelayMs,
            });
        }

        if self.m_u32ResponseDelayMs > self.m_u32P4ServerMaxMs {
            return Err(TimingValidationError::ResponseDelayBeyondP4 {
                u32DelayMs: self.m_u32ResponseDelayMs,
                u32P4Ms: self.m_u32P4ServerMaxMs,
            });
        }

        self.ValidateForcedResponsePending()
    }

    /// The rules that only apply when the operator forces a ResponsePending sequence.
    fn ValidateForcedResponsePending(&self) -> Result<(), TimingValidationError> {
        if !self.m_bForceResponsePending {
            return Ok(());
        }

        if self.m_u8ForcedResponsePendingCount == 0 {
            return Err(TimingValidationError::ForcedResponsePendingCountIsZero);
        }

        if self.m_u8ForcedResponsePendingCount > c_u8MaxForcedResponsePendingCount {
            return Err(TimingValidationError::ForcedResponsePendingCountTooLarge {
                u8Value: self.m_u8ForcedResponsePendingCount,
                u8Limit: c_u8MaxForcedResponsePendingCount,
            });
        }

        if self.m_u32P2StarServerMaxMs == 0 {
            return Err(TimingValidationError::ForcedResponsePendingWithoutP2Star);
        }

        if self.m_u32P4ServerMaxMs == self.m_u32P2ServerMaxMs {
            return Err(
                TimingValidationError::ForcedResponsePendingWithP4EqualToP2 {
                    u32P2Ms: self.m_u32P2ServerMaxMs,
                },
            );
        }

        Ok(())
    }
}

/// How an ECU is addressed on CAN (ISO 15765-2). The MVP simulates physically-addressed
/// UDS-on-CAN only; the variant is carried so a later phase can add the other modes without
/// reshaping the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CanAddressingMode {
    /// Normal 11-bit addressing — one CAN ID per direction, no address byte in the payload.
    #[default]
    Normal11Bit,
    /// Normal fixed 29-bit addressing — 0x18DA<target><source>, tester source address 0xF1.
    NormalFixed29Bit,
}

/// The pair of CAN identifiers a physically-addressed ECU uses: the identifier a tester sends
/// requests on, and the identifier the ECU answers on.
///
/// Both are stored as `u32` because 29-bit (extended) identifiers do not fit in a `u16`.
/// `m_confidence` records how the pair was established: `Observed` when both identifiers were
/// actually seen in a trace, `Inferred` when one of them was derived from the other by a
/// convention (e.g. response = request + 8) rather than witnessed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanAddress {
    /// CAN identifier the tester sends requests on (e.g. 0x7E0).
    pub m_u32RequestCanId: u32,
    /// CAN identifier the ECU sends responses on (e.g. 0x7E8).
    pub m_u32ResponseCanId: u32,
    /// Functional (broadcast) identifier this ECU also accepts requests on, if any: 0x7DF for
    /// legislated 11-bit addressing, 0x18DB33F1 for 29-bit normal fixed. `None` when nothing
    /// in the sources or the standards says the ECU listens functionally. Defaulted on
    /// deserialization so models written before this field existed still load.
    #[serde(default)]
    pub m_optU32FunctionalCanId: Option<u32>,
    /// Addressing mode the two identifiers follow.
    pub m_addressingMode: CanAddressingMode,
    /// How the pair was established.
    pub m_confidence: Confidence,
}

/// Offset between an 11-bit UDS request identifier and its response identifier (0x7E0 -> 0x7E8).
///
/// ISO 15765-4 fixes this pairing for the legislated range 0x7E0..=0x7E7 only. Outside that
/// range identifier pairs are OEM-specific and follow no derivable rule (0x745 -> 0x765 is a
/// real, observed example), so the offset must never be applied there.
pub const c_u32Response11BitOffset: u32 = 0x08;

/// Lowest legislated 11-bit UDS request identifier (ISO 15765-4).
pub const c_u32LegislatedRequestCanIdFirst: u32 = 0x7E0;
/// Highest legislated 11-bit UDS request identifier (ISO 15765-4).
pub const c_u32LegislatedRequestCanIdLast: u32 = 0x7E7;

/// The 11-bit functional (broadcast) request identifier every legislated ECU listens on
/// (ISO 15765-4). It is a listen address shared by all ECUs, never one ECU's own request
/// identifier.
pub const c_u32Functional11BitCanId: u32 = 0x7DF;

/// The 29-bit normal-fixed functional request identifier: target address 0x33 (all ECUs),
/// source address 0xF1 (tester), N_TAtype 0xDB (functional) — ISO 15765-2.
pub const c_u32FunctionalNormalFixed29BitCanId: u32 = 0x18DB_33F1;

impl CanAddress {
    /// Build a normal 11-bit address pair from both observed identifiers.
    pub fn NewObserved11Bit(u32RequestCanId: u32, u32ResponseCanId: u32) -> Self {
        CanAddress {
            m_u32RequestCanId: u32RequestCanId,
            m_u32ResponseCanId: u32ResponseCanId,
            m_optU32FunctionalCanId: DefaultFunctionalCanId(
                u32RequestCanId,
                CanAddressingMode::Normal11Bit,
            ),
            m_addressingMode: CanAddressingMode::Normal11Bit,
            m_confidence: Confidence::Observed,
        }
    }

    /// Build an address pair a user stated rather than one observed on a bus.
    ///
    /// `Confirmed` rather than `Observed`: nothing was seen on a bus, but nothing was guessed
    /// either — the identifiers came from someone who knows the vehicle, which is the same
    /// standing a specification has (README §7).
    pub fn NewSpecified(
        u32RequestCanId: u32,
        u32ResponseCanId: u32,
        mode: CanAddressingMode,
    ) -> Self {
        CanAddress {
            m_u32RequestCanId: u32RequestCanId,
            m_u32ResponseCanId: u32ResponseCanId,
            m_optU32FunctionalCanId: DefaultFunctionalCanId(u32RequestCanId, mode),
            m_addressingMode: mode,
            m_confidence: Confidence::Confirmed,
        }
    }

    /// True when the identifiers are 29-bit extended.
    ///
    /// Derived from the addressing mode rather than from the identifier value: a value below
    /// 0x800 may legally be transmitted in an extended frame, so the value alone cannot decide.
    pub fn IsExtendedId(&self) -> bool {
        self.m_addressingMode == CanAddressingMode::NormalFixed29Bit
    }

    /// True when this ECU listens on the given functional (broadcast) identifier.
    pub fn ListensFunctionallyOn(&self, u32CanId: u32) -> bool {
        self.m_optU32FunctionalCanId == Some(u32CanId)
    }
}

/// The functional identifier an ECU on this request identifier is required to listen on, or
/// `None` when no standard mandates one.
///
/// ISO 15765-4 requires every ECU in the legislated 11-bit range to accept 0x7DF, and
/// ISO 15765-2 defines 0x18DB33F1 for 29-bit normal-fixed addressing. An OEM-specific 11-bit
/// pair (e.g. 0x745/0x765) is outside both standards, so nothing can be assumed for it.
pub fn DefaultFunctionalCanId(u32RequestCanId: u32, mode: CanAddressingMode) -> Option<u32> {
    match mode {
        CanAddressingMode::NormalFixed29Bit => Some(c_u32FunctionalNormalFixed29BitCanId),
        CanAddressingMode::Normal11Bit => {
            let bIsLegislated = (c_u32LegislatedRequestCanIdFirst
                ..=c_u32LegislatedRequestCanIdLast)
                .contains(&u32RequestCanId);
            if bIsLegislated {
                Some(c_u32Functional11BitCanId)
            } else {
                None
            }
        }
    }
}

/// Copy a run of bytes out of the request into the response, so one override can answer a
/// whole family of requests without lying about which one it answered.
///
/// A wildcard override on ReadDataByIdentifier is the motivating case: the response must echo
/// the DID that was asked for (ISO 14229-1 clause 10.2), and a fixed response would answer a
/// read of 0xF18C with 0xF190 in it — which any tester correlating on the identifier rejects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EchoSpan {
    /// Where the run starts in the request.
    pub m_uRequestOffset: usize,
    /// How many bytes to copy.
    pub m_uLength: usize,
    /// Where the run lands in the response.
    pub m_uResponseOffset: usize,
}

/// What an override does when it matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OverrideAction {
    /// Send these bytes instead of whatever the protocol produced.
    Substitute {
        /// The response bytes, before any echo spans are applied.
        m_vecResponse: Vec<u8>,
        /// Runs of the request to copy into the response.
        #[serde(default)]
        m_vecEchoSpans: Vec<EchoSpan>,
    },
    /// Send nothing at all.
    ///
    /// A distinct action rather than an empty substitution: "present but silent for this one
    /// request" is a real failure that no negative response can express, and the engine
    /// already distinguishes several kinds of silence — a fourth has to be nameable in the log
    /// or an operator debugging it cannot tell which they are looking at.
    Suppress,
}

/// A user-defined answer to a particular request.
///
/// Overrides exist because declaring a service supported does not implement it: the bundled
/// UDS plugin answers seven services, and for everything else — WriteDataByIdentifier,
/// RequestDownload, ClearDiagnosticInformation — an override is the only way to get a positive
/// response at all.
///
/// An override changes **what the ECU says, not what it does**: the protocol still runs, and
/// the session and security state machines still behave normally. ISO 14229-1 clause 8.2 makes
/// the same separation for the suppressPosRspMsgIndicationBit — "the execution of the service
/// must be completely passed" even when nothing is transmitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseOverride {
    /// The request bytes to match. Bytes where the mask is zero are ignored.
    pub m_vecRequestPattern: Vec<u8>,
    /// One mask byte per pattern byte: 0xFF means "must match", 0x00 means "any value".
    pub m_vecRequestMask: Vec<u8>,
    /// When true the pattern is a prefix and a longer request still matches — needed for
    /// requests with a variable tail, such as a SecurityAccess key or a TransferData block.
    /// Off by default, so a short pattern cannot accidentally become a catch-all.
    #[serde(default)]
    pub m_bMatchTrailingBytes: bool,
    /// What to send when it matches.
    pub m_action: OverrideAction,
    /// Off keeps the override in the model without applying it, so an operator can park one
    /// mid-investigation rather than retyping it.
    pub m_bIsEnabled: bool,
    /// Apply even when the protocol suppressed its response because the tester set the
    /// suppressPosRspMsgIndicationBit. Off by default: the tester asked not to be answered and
    /// is not listening, so answering anyway is fault injection, not a fix.
    #[serde(default)]
    pub m_bRespondEvenIfSuppressed: bool,
    /// Why this override exists, for whoever reads the model next.
    #[serde(default)]
    pub m_strNote: String,
}

impl ResponseOverride {
    /// Build an override that matches one exact request.
    pub fn NewExact(vecRequest: &[u8], action: OverrideAction) -> Self {
        ResponseOverride {
            m_vecRequestPattern: vecRequest.to_vec(),
            m_vecRequestMask: vec![0xFF; vecRequest.len()],
            m_bMatchTrailingBytes: false,
            m_action: action,
            m_bIsEnabled: true,
            m_bRespondEvenIfSuppressed: false,
            m_strNote: String::new(),
        }
    }

    /// True when this override applies to the given request.
    pub fn Matches(&self, vecRequest: &[u8]) -> bool {
        if !self.m_bIsEnabled || self.m_vecRequestPattern.is_empty() {
            return false;
        }
        if self.m_vecRequestMask.len() != self.m_vecRequestPattern.len() {
            return false;
        }

        let bIsLengthAcceptable = if self.m_bMatchTrailingBytes {
            vecRequest.len() >= self.m_vecRequestPattern.len()
        } else {
            vecRequest.len() == self.m_vecRequestPattern.len()
        };
        if !bIsLengthAcceptable {
            return false;
        }

        // Walk pattern, mask and request together. `vecRequest` may be longer when the
        // pattern is a prefix; zip stops at the pattern, which is exactly right.
        let itBytes = self
            .m_vecRequestPattern
            .iter()
            .zip(&self.m_vecRequestMask)
            .zip(vecRequest);
        for ((byPattern, byMask), byActual) in itBytes {
            if (*byActual & *byMask) != (*byPattern & *byMask) {
                return false;
            }
        }
        true
    }

    /// How specific this override is, most specific first when sorted descending.
    ///
    /// More fixed bytes beats fewer, then a longer pattern, then an anchored pattern beats a
    /// prefix. Deliberately not "last one wins": reordering the list in a UI must never change
    /// behaviour silently.
    pub fn Specificity(&self) -> (usize, usize, bool) {
        let uFixedBytes = self
            .m_vecRequestMask
            .iter()
            .filter(|byMask| **byMask != 0x00)
            .count();
        (
            uFixedBytes,
            self.m_vecRequestPattern.len(),
            !self.m_bMatchTrailingBytes,
        )
    }

    /// The response bytes this override produces for a request, with echo spans applied.
    /// `None` when the override suppresses the response instead.
    pub fn BuildResponse(&self, vecRequest: &[u8]) -> Option<Vec<u8>> {
        let (vecTemplate, vecEchoSpans) = match &self.m_action {
            OverrideAction::Suppress => return None,
            OverrideAction::Substitute {
                m_vecResponse,
                m_vecEchoSpans,
            } => (m_vecResponse, m_vecEchoSpans),
        };

        let mut vecResponse = vecTemplate.clone();
        for span in vecEchoSpans {
            ApplyEchoSpan(span, vecRequest, &mut vecResponse);
        }
        Some(vecResponse)
    }

    /// The request service identifier this override answers, if the pattern states one.
    pub fn RequestServiceId(&self) -> Option<u8> {
        let byFirstMask = *self.m_vecRequestMask.first()?;
        if byFirstMask != 0xFF {
            return None;
        }
        self.m_vecRequestPattern.first().copied()
    }
}

/// Copy one run of request bytes into the response, ignoring a span that would not fit rather
/// than growing the response — a span past the end is a configuration error, and silently
/// extending the response would hide it.
fn ApplyEchoSpan(span: &EchoSpan, vecRequest: &[u8], vecResponse: &mut [u8]) {
    let uRequestEnd = span.m_uRequestOffset + span.m_uLength;
    let uResponseEnd = span.m_uResponseOffset + span.m_uLength;
    if uRequestEnd > vecRequest.len() || uResponseEnd > vecResponse.len() {
        return;
    }

    let vecCopied = vecRequest[span.m_uRequestOffset..uRequestEnd].to_vec();
    vecResponse[span.m_uResponseOffset..uResponseEnd].copy_from_slice(&vecCopied);
}

/// Offset between a request service identifier and its positive response (ISO 14229-1
/// Table 2: bit 6 of the SID is the response flag).
pub const c_byPositiveResponseOffset: u8 = 0x40;
/// First byte of a negative response.
pub const c_byNegativeResponseSid: u8 = 0x7F;
/// NRC 0x78 requestCorrectlyReceived-ResponsePending.
pub const c_byNrcResponsePending: u8 = 0x78;

/// Why a response override was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverrideValidationError {
    /// Nothing to match on.
    #[error("the request pattern is empty")]
    EmptyRequestPattern,

    /// One mask byte per pattern byte, or the match is undefined.
    #[error("the request pattern is {uPatternLength} bytes but the mask is {uMaskLength}")]
    MaskLengthMismatch {
        uPatternLength: usize,
        uMaskLength: usize,
    },

    /// A pattern that fixes nothing answers everything, turning the ECU into a replayer.
    #[error("every byte of the request pattern is a wildcard, so it would answer every request")]
    EverythingWildcarded,

    /// The service identifier has to be fixed, or the override cannot be reasoned about.
    #[error(
        "the first pattern byte (the service identifier) must be matched exactly, not wildcarded"
    )]
    ServiceIdWildcarded,

    /// Requests live in two ranges; anything else is a response or not UDS at all.
    #[error("0x{byServiceId:02X} is not a UDS request service identifier (ISO 14229-1 clause 7.3 defines 0x10..=0x3E and 0x83..=0x88)")]
    NotARequestServiceId { byServiceId: u8 },

    /// Silence is its own action, so the log can name which kind of silence it is.
    #[error(
        "a substituting override needs response bytes; use the suppress action to send nothing"
    )]
    EmptySubstituteResponse,

    /// The response has to answer the request it replaces.
    #[error("the response starts with 0x{byResponseSid:02X}, which is neither the positive response to 0x{byRequestSid:02X} (0x{byExpected:02X}) nor a negative response (0x7F)")]
    ResponseSidMismatch {
        byResponseSid: u8,
        byRequestSid: u8,
        byExpected: u8,
    },

    /// A negative response is exactly three bytes (ISO 14229-1 clause 7.4).
    #[error("a negative response is exactly 3 bytes (7F <sid> <nrc>), but this one is {uLength}")]
    MalformedNegativeResponse { uLength: usize },

    /// The echoed service identifier has to be the one that was requested.
    #[error("the negative response echoes service 0x{byEchoedSid:02X} but the request is 0x{byRequestSid:02X}")]
    NegativeResponseSidMismatch { byEchoedSid: u8, byRequestSid: u8 },

    /// ResponsePending is the session layer's to send, and it obliges a final response.
    #[error("NRC 0x78 cannot be an override: it is a promise to answer later, and the timing layer owns that sequence")]
    ResponsePendingAsOverride,

    /// An echo span that reaches past either buffer is a configuration mistake.
    #[error("an echo span reads request bytes {uRequestOffset}..{uRequestEnd}, which a request matching this pattern may not have")]
    EchoSpanOutsideRequest {
        uRequestOffset: usize,
        uRequestEnd: usize,
    },

    /// Same, on the response side.
    #[error("an echo span writes response bytes {uResponseOffset}..{uResponseEnd}, past the end of a {uResponseLength}-byte response")]
    EchoSpanOutsideResponse {
        uResponseOffset: usize,
        uResponseEnd: usize,
        uResponseLength: usize,
    },
}

impl ResponseOverride {
    /// Check that this override could be a real exchange on a real bus.
    ///
    /// Rejects what cannot be true — a response that does not answer its request, a negative
    /// response of the wrong shape, an NRC the session layer owns. It does **not** reject
    /// implausible-but-possible behaviour: an ECU that refuses a read it should allow is a
    /// legitimate fault to inject, and the same "reject values, execute behaviours" rule the
    /// timing layer follows applies here.
    pub fn Validate(&self) -> Result<(), OverrideValidationError> {
        if self.m_vecRequestPattern.is_empty() {
            return Err(OverrideValidationError::EmptyRequestPattern);
        }
        if self.m_vecRequestMask.len() != self.m_vecRequestPattern.len() {
            return Err(OverrideValidationError::MaskLengthMismatch {
                uPatternLength: self.m_vecRequestPattern.len(),
                uMaskLength: self.m_vecRequestMask.len(),
            });
        }
        if self.m_vecRequestMask.iter().all(|byMask| *byMask == 0x00) {
            return Err(OverrideValidationError::EverythingWildcarded);
        }
        if self.m_vecRequestMask[0] != 0xFF {
            return Err(OverrideValidationError::ServiceIdWildcarded);
        }

        let byRequestSid = self.m_vecRequestPattern[0];
        if !IsRequestServiceId(byRequestSid) {
            return Err(OverrideValidationError::NotARequestServiceId {
                byServiceId: byRequestSid,
            });
        }

        self.ValidateAction(byRequestSid)
    }

    /// The half of validation that depends on what the override sends.
    fn ValidateAction(&self, byRequestSid: u8) -> Result<(), OverrideValidationError> {
        let (vecResponse, vecEchoSpans) = match &self.m_action {
            OverrideAction::Suppress => return Ok(()),
            OverrideAction::Substitute {
                m_vecResponse,
                m_vecEchoSpans,
            } => (m_vecResponse, m_vecEchoSpans),
        };

        if vecResponse.is_empty() {
            return Err(OverrideValidationError::EmptySubstituteResponse);
        }

        if vecResponse[0] == c_byNegativeResponseSid {
            ValidateNegativeResponse(vecResponse, byRequestSid)?;
        } else {
            let byExpected = byRequestSid.wrapping_add(c_byPositiveResponseOffset);
            if vecResponse[0] != byExpected {
                return Err(OverrideValidationError::ResponseSidMismatch {
                    byResponseSid: vecResponse[0],
                    byRequestSid,
                    byExpected,
                });
            }
        }

        for span in vecEchoSpans {
            ValidateEchoSpan(span, &self.m_vecRequestPattern, vecResponse)?;
        }
        Ok(())
    }
}

/// A negative response is `7F <sid> <nrc>`, echoing the service it refuses.
fn ValidateNegativeResponse(
    vecResponse: &[u8],
    byRequestSid: u8,
) -> Result<(), OverrideValidationError> {
    if vecResponse.len() != 3 {
        return Err(OverrideValidationError::MalformedNegativeResponse {
            uLength: vecResponse.len(),
        });
    }
    if vecResponse[1] != byRequestSid {
        return Err(OverrideValidationError::NegativeResponseSidMismatch {
            byEchoedSid: vecResponse[1],
            byRequestSid,
        });
    }
    if vecResponse[2] == c_byNrcResponsePending {
        return Err(OverrideValidationError::ResponsePendingAsOverride);
    }
    Ok(())
}

/// An echo span must fit both the shortest matching request and the response template.
fn ValidateEchoSpan(
    span: &EchoSpan,
    vecRequestPattern: &[u8],
    vecResponse: &[u8],
) -> Result<(), OverrideValidationError> {
    let uRequestEnd = span.m_uRequestOffset + span.m_uLength;
    if uRequestEnd > vecRequestPattern.len() {
        return Err(OverrideValidationError::EchoSpanOutsideRequest {
            uRequestOffset: span.m_uRequestOffset,
            uRequestEnd,
        });
    }

    let uResponseEnd = span.m_uResponseOffset + span.m_uLength;
    if uResponseEnd > vecResponse.len() {
        return Err(OverrideValidationError::EchoSpanOutsideResponse {
            uResponseOffset: span.m_uResponseOffset,
            uResponseEnd,
            uResponseLength: vecResponse.len(),
        });
    }
    Ok(())
}

/// True for a UDS request service identifier (ISO 14229-1 clause 7.3, Table 2).
pub fn IsRequestServiceId(byServiceId: u8) -> bool {
    (0x10..=0x3E).contains(&byServiceId) || (0x83..=0x88).contains(&byServiceId)
}

/// A single virtual ECU's static diagnostic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ecu {
    /// Human-readable name, e.g. "Engine_ECU".
    pub m_strName: String,
    /// UDS logical/diagnostic address (used for DoIP and addressing later).
    pub m_u16LogicalAddress: u16,
    /// CAN identifiers this ECU is reached on. `None` means the ECU's CAN addressing is not
    /// known (e.g. a hand-built model, or one imported from a source that carries no CAN
    /// addressing); such an ECU cannot be routed to on CAN. Defaulted on deserialization so
    /// models written before this field existed still load.
    #[serde(default)]
    pub m_optCanAddress: Option<CanAddress>,
    /// Service IDs (request SIDs) this ECU supports, e.g. 0x10, 0x22, 0x27.
    pub m_vecSupportedServices: Vec<u8>,
    /// Sessions this ECU can enter.
    pub m_vecSupportedSessions: Vec<SessionType>,
    /// DIDs keyed by identifier, so lookups for service 0x22 are ordered and fast.
    pub m_mapDids: BTreeMap<u16, DataIdentifier>,
    /// Stored DTCs reported by service 0x19.
    pub m_vecDtcs: Vec<DiagnosticTroubleCode>,
    /// Security levels this ECU supports (service 0x27).
    pub m_vecSecurityLevels: Vec<SecurityLevel>,
    /// Timing parameters.
    pub m_timing: EcuTiming,
    /// Which services are reachable in which session, keyed by the session's sub-function byte.
    ///
    /// Empty — the default — means every supported service works in every supported session,
    /// which is what the engine did before this existed. A session that *is* listed restricts
    /// what it allows: a request for a service outside its list is refused with NRC 0x7F
    /// serviceNotSupportedInActiveSession, which is how a real ECU keeps flashing and
    /// actuation out of the default session.
    #[serde(default)]
    pub m_mapSessionServices: BTreeMap<u8, Vec<u8>>,

    /// Networks this ECU forwards diagnostics onto, by id.
    ///
    /// This is what makes an ECU a gateway, and it is the piece that lets a topology show
    /// depth rather than a flat list: a tester reaches an ECU on one of these networks only by
    /// going through this one. Empty for an ordinary ECU.
    #[serde(default)]
    pub m_vecGatewayForNetworkIds: Vec<String>,

    /// Which network this ECU sits on, by id. `None` means nobody has said — not "the default
    /// bus". An ECU reconstructed from a log is always `None`, because a capture cannot
    /// observe bus membership.
    #[serde(default)]
    pub m_optStrNetworkId: Option<String>,
    /// True when `m_u16LogicalAddress` is a real DoIP logical address a tester can route to,
    /// rather than the placeholder a CAN-only ECU carries.
    ///
    /// An ECU may be reachable on CAN, on DoIP, or on both — a gateway usually is both, since
    /// that is what makes it a gateway. Keeping this a flag rather than a second address field
    /// means there is only ever one logical address, which cannot disagree with itself.
    #[serde(default)]
    pub m_bHasDoIpAddress: bool,

    /// User-defined answers to particular requests, tried most-specific-first before the
    /// protocol's own response is used. Defaulted on deserialization so models written before
    /// this field existed still load.
    #[serde(default)]
    pub m_vecResponseOverrides: Vec<ResponseOverride>,
}

impl Ecu {
    /// Create an ECU with the given name/address and no capabilities yet. Callers populate
    /// the collections explicitly, which keeps ECU construction readable at call sites.
    pub fn New(strName: &str, u16LogicalAddress: u16) -> Self {
        Ecu {
            m_strName: strName.to_string(),
            m_u16LogicalAddress: u16LogicalAddress,
            m_optCanAddress: None,
            m_vecSupportedServices: Vec::new(),
            m_vecSupportedSessions: vec![SessionType::Default],
            m_mapDids: BTreeMap::new(),
            m_vecDtcs: Vec::new(),
            m_vecSecurityLevels: Vec::new(),
            m_timing: EcuTiming::default(),
            m_mapSessionServices: BTreeMap::new(),
            m_vecGatewayForNetworkIds: Vec::new(),
            m_bHasDoIpAddress: false,
            m_optStrNetworkId: None,
            m_vecResponseOverrides: Vec::new(),
        }
    }

    /// The override that answers this request, or `None` if none applies.
    ///
    /// Most specific wins, so a rule for one exact request always beats a wildcard family.
    pub fn FindMatchingOverride(&self, vecRequest: &[u8]) -> Option<&ResponseOverride> {
        self.m_vecResponseOverrides
            .iter()
            .filter(|candidate| candidate.Matches(vecRequest))
            .max_by_key(|candidate| candidate.Specificity())
    }

    /// True if the ECU advertises support for the given request service id.
    pub fn IsServiceSupported(&self, byServiceId: u8) -> bool {
        self.m_vecSupportedServices.contains(&byServiceId)
    }

    /// True if the service can be reached from the session the ECU is currently in.
    ///
    /// An ECU that says nothing about sessions allows everything it supports, everywhere. One
    /// that restricts a session allows only what that session lists — and a session it does
    /// not mention at all is unrestricted, so adding a rule for `extended` does not silently
    /// lock down `default` too.
    pub fn IsServiceAllowedInSession(&self, byServiceId: u8, bySession: u8) -> bool {
        match self.m_mapSessionServices.get(&bySession) {
            Some(vecAllowed) => vecAllowed.contains(&byServiceId),
            None => true,
        }
    }

    /// True if the ECU can enter the given session.
    pub fn IsSessionSupported(&self, session: SessionType) -> bool {
        self.m_vecSupportedSessions.contains(&session)
    }

    /// Look up a DID's configured value.
    pub fn FindDid(&self, u16Id: u16) -> Option<&DataIdentifier> {
        self.m_mapDids.get(&u16Id)
    }

    /// Find the security level identified by a requestSeed sub-function.
    pub fn FindSecurityLevelByRequestSeed(&self, bySubFunction: u8) -> Option<&SecurityLevel> {
        self.m_vecSecurityLevels
            .iter()
            .find(|level| level.m_byRequestSeedSubFunction == bySubFunction)
    }

    /// Find the security level whose sendKey sub-function matches the given value.
    pub fn FindSecurityLevelBySendKey(&self, bySubFunction: u8) -> Option<&SecurityLevel> {
        self.m_vecSecurityLevels
            .iter()
            .find(|level| level.SendKeySubFunction() == bySubFunction)
    }
}

/// What kind of link a network is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NetworkKind {
    /// Classic CAN, up to 8 bytes per frame.
    CanClassic,
    /// CAN-FD, which carries longer frames and a second bit rate for the data phase.
    CanFd,
    /// Ethernet carrying DoIP.
    EthernetDoIp,
    /// The link exists but nothing said what it is.
    #[default]
    Unknown,
}

/// One bus in the vehicle.
///
/// A network only exists when something actually stated it. Reconstruction cannot invent one:
/// a tester-side capture sees a single connector and cannot tell whether two ECUs share a wire
/// or sit behind a gateway (ADR 0006 §4), so a vehicle built from a log has no networks at all
/// and its topology is drawn as one reachability set. A simulation file, by contrast, is
/// written by someone who knows the vehicle, so what it says about buses is `Confirmed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// Stable key ECUs refer to, e.g. "powertrain".
    pub m_strId: String,
    /// What to call it on screen, e.g. "Powertrain CAN".
    pub m_strName: String,
    /// What kind of link it is.
    pub m_kind: NetworkKind,
    /// Arbitration bit rate, when known. Never defaulted to a plausible-looking 500 kbit/s:
    /// a capture cannot observe it, so guessing would turn "unknown" into a claim.
    #[serde(default)]
    pub m_optU32BitrateBps: Option<u32>,
    /// The CAN-FD data-phase bit rate, when the link has one.
    #[serde(default)]
    pub m_optU32DataBitrateBps: Option<u32>,
    /// True for the link a tester actually connects to — the diagnostic socket, or the
    /// Ethernet interface a DoIP tester opens. Everything else is reached *through* something.
    #[serde(default)]
    pub m_bIsDiagnosticEntryPoint: bool,
    /// How the existence of this network was established.
    pub m_confidence: Confidence,
}

/// The whole reconstructed/simulated vehicle: its buses and the ECUs on them.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Vehicle {
    /// Vehicle name/identifier.
    pub m_strName: String,
    /// All ECUs in the vehicle.
    pub m_vecEcus: Vec<Ecu>,
    /// The buses, when something stated them. Empty means nobody has said how these ECUs are
    /// connected — which is the honest state after reconstructing from a log, and must be
    /// rendered as "unknown" rather than filled in with a default bus.
    #[serde(default)]
    pub m_vecNetworks: Vec<Network>,
}

impl Vehicle {
    /// Find a network by the id ECUs refer to it with.
    pub fn FindNetwork(&self, strNetworkId: &str) -> Option<&Network> {
        self.m_vecNetworks
            .iter()
            .find(|network| network.m_strId == strNetworkId)
    }

    /// The ECU that forwards diagnostics onto a network, if one does.
    pub fn FindGatewayForNetwork(&self, strNetworkId: &str) -> Option<&Ecu> {
        self.m_vecEcus.iter().find(|ecu| {
            ecu.m_vecGatewayForNetworkIds
                .iter()
                .any(|strBehind| strBehind == strNetworkId)
        })
    }

    /// The networks a tester can attach to directly.
    ///
    /// A network nothing gateways onto and that is not marked as an entry point is
    /// unreachable, which is worth being able to see rather than hiding.
    pub fn EntryPointNetworks(&self) -> Vec<&Network> {
        self.m_vecNetworks
            .iter()
            .filter(|network| network.m_bIsDiagnosticEntryPoint)
            .collect()
    }
}

impl Vehicle {
    /// Serialize to pretty JSON for persistence/inspection.
    pub fn ToJson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Load a vehicle from JSON.
    pub fn FromJson(strJson: &str) -> Result<Vehicle, serde_json::Error> {
        serde_json::from_str(strJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_type_maps_to_and_from_sub_function() {
        assert_eq!(SessionType::Extended.ToSubFunction(), 0x03);
        assert_eq!(
            SessionType::FromSubFunction(0x02),
            Some(SessionType::Programming)
        );
        assert_eq!(SessionType::FromSubFunction(0xFF), None);
    }

    #[test]
    fn ecu_lookups_work() {
        let mut ecu = Ecu::New("Engine_ECU", 0x1001);
        ecu.m_vecSupportedServices.push(0x22);
        ecu.m_mapDids.insert(
            0xF190,
            DataIdentifier {
                m_u16Id: 0xF190,
                m_vecValue: b"VIN0123456789".to_vec(),
                m_confidence: Confidence::Observed,
            },
        );

        assert!(ecu.IsServiceSupported(0x22));
        assert!(!ecu.IsServiceSupported(0x27));
        assert!(ecu.IsSessionSupported(SessionType::Default));
        assert_eq!(ecu.FindDid(0xF190).unwrap().m_vecValue, b"VIN0123456789");
        assert!(ecu.FindDid(0x1234).is_none());
    }

    #[test]
    fn security_level_pairs_seed_and_key_sub_functions() {
        let level = SecurityLevel {
            m_byRequestSeedSubFunction: 0x01,
            m_vecSeed: vec![0x11, 0x22],
            m_vecExpectedKey: vec![0x33, 0x44],
        };
        assert_eq!(level.SendKeySubFunction(), 0x02);
    }

    fn ExactOverride(vecRequest: &[u8], vecResponse: &[u8]) -> ResponseOverride {
        ResponseOverride::NewExact(
            vecRequest,
            OverrideAction::Substitute {
                m_vecResponse: vecResponse.to_vec(),
                m_vecEchoSpans: Vec::new(),
            },
        )
    }

    #[test]
    fn a_well_formed_override_validates() {
        assert_eq!(
            ExactOverride(&[0x2E, 0xF1, 0x90, 0x01], &[0x6E, 0xF1, 0x90]).Validate(),
            Ok(())
        );
        // A refusal is just as valid as a positive answer.
        assert_eq!(
            ExactOverride(&[0x22, 0xF1, 0x90], &[0x7F, 0x22, 0x33]).Validate(),
            Ok(())
        );
    }

    #[test]
    fn an_override_whose_response_does_not_answer_the_request_is_refused() {
        // 0x6E answers 0x2E, not 0x22.
        assert!(matches!(
            ExactOverride(&[0x22, 0xF1, 0x90], &[0x6E, 0xF1, 0x90]).Validate(),
            Err(OverrideValidationError::ResponseSidMismatch { .. })
        ));
    }

    #[test]
    fn a_malformed_negative_response_is_refused() {
        // A negative response is exactly three bytes.
        assert!(matches!(
            ExactOverride(&[0x22, 0xF1, 0x90], &[0x7F, 0x22]).Validate(),
            Err(OverrideValidationError::MalformedNegativeResponse { .. })
        ));
        // And it echoes the service it refuses.
        assert!(matches!(
            ExactOverride(&[0x22, 0xF1, 0x90], &[0x7F, 0x2E, 0x33]).Validate(),
            Err(OverrideValidationError::NegativeResponseSidMismatch { .. })
        ));
    }

    #[test]
    fn response_pending_cannot_be_an_override() {
        // NRC 0x78 is a promise to answer later, and only the timing layer can keep it.
        assert_eq!(
            ExactOverride(&[0x22, 0xF1, 0x90], &[0x7F, 0x22, 0x78]).Validate(),
            Err(OverrideValidationError::ResponsePendingAsOverride)
        );
    }

    #[test]
    fn a_pattern_that_matches_everything_is_refused() {
        let catchAll = ResponseOverride {
            m_vecRequestPattern: vec![0x00, 0x00],
            m_vecRequestMask: vec![0x00, 0x00],
            m_bMatchTrailingBytes: true,
            m_action: OverrideAction::Suppress,
            m_bIsEnabled: true,
            m_bRespondEvenIfSuppressed: false,
            m_strNote: String::new(),
        };
        assert_eq!(
            catchAll.Validate(),
            Err(OverrideValidationError::EverythingWildcarded)
        );
    }

    #[test]
    fn a_response_byte_is_not_a_request() {
        // 0x62 is the positive response to 0x22, not something a tester sends.
        assert!(matches!(
            ExactOverride(&[0x62, 0xF1, 0x90], &[0xA2]).Validate(),
            Err(OverrideValidationError::NotARequestServiceId { .. })
        ));
    }

    #[test]
    fn an_echo_span_past_the_end_of_the_response_is_refused() {
        let overrideRule = ResponseOverride {
            m_vecRequestPattern: vec![0x22, 0x00, 0x00],
            m_vecRequestMask: vec![0xFF, 0x00, 0x00],
            m_bMatchTrailingBytes: false,
            m_action: OverrideAction::Substitute {
                m_vecResponse: vec![0x62, 0x00],
                m_vecEchoSpans: vec![EchoSpan {
                    m_uRequestOffset: 1,
                    m_uLength: 2,
                    m_uResponseOffset: 1,
                }],
            },
            m_bIsEnabled: true,
            m_bRespondEvenIfSuppressed: false,
            m_strNote: String::new(),
        };
        assert!(matches!(
            overrideRule.Validate(),
            Err(OverrideValidationError::EchoSpanOutsideResponse { .. })
        ));
    }

    #[test]
    fn a_more_specific_override_sorts_above_a_wildcard() {
        let wildcard = ResponseOverride {
            m_vecRequestPattern: vec![0x22, 0x00, 0x00],
            m_vecRequestMask: vec![0xFF, 0x00, 0x00],
            ..ExactOverride(&[0x22, 0xF1, 0x90], &[0x62, 0xF1, 0x90])
        };
        let exact = ExactOverride(&[0x22, 0xF1, 0x90], &[0x62, 0xF1, 0x90]);

        assert!(exact.Specificity() > wildcard.Specificity());
        assert!(exact.Matches(&[0x22, 0xF1, 0x90]));
        assert!(wildcard.Matches(&[0x22, 0xF1, 0x90]));
        assert!(!exact.Matches(&[0x22, 0xF1, 0x8C]));
        assert!(wildcard.Matches(&[0x22, 0xF1, 0x8C]));
    }

    #[test]
    fn vehicle_round_trips_through_json() {
        let mut vehicle = Vehicle {
            m_strName: "TestVehicle".to_string(),
            m_vecEcus: Vec::new(),
            m_vecNetworks: Vec::new(),
        };
        vehicle.m_vecEcus.push(Ecu::New("Engine_ECU", 0x1001));

        let strJson = vehicle.ToJson().unwrap();
        let loaded = Vehicle::FromJson(&strJson).unwrap();

        assert_eq!(loaded.m_strName, "TestVehicle");
        assert_eq!(loaded.m_vecEcus.len(), 1);
        assert_eq!(loaded.m_vecEcus[0].m_u16LogicalAddress, 0x1001);
    }
}

// ==========================================================================================
// Vehicle wiring: which ECU sits behind which gateway, and on what.
//
// A vehicle is not a flat list of ECUs. A tester attaches to one link — a diagnostic socket or
// an Ethernet interface — and reaches everything else *through* something. The types below are
// the one place that fact is worked out, so a simulation file, a hand-built vehicle and a
// log-reconstructed one all get the same answer rather than three approximations of it.
// ==========================================================================================

/// Why a vehicle's declared wiring does not describe something that could exist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    /// An ECU sits on, or gateways onto, a network the vehicle does not define.
    #[error(
        "ECU '{strEcuName}' refers to network '{strNetworkId}', which this vehicle does not define"
    )]
    UnknownNetwork {
        /// The ECU making the reference.
        strEcuName: String,
        /// The network id nothing defines.
        strNetworkId: String,
    },

    /// An ECU gateways onto the network it is already on, which is a loop of length one.
    #[error("ECU '{strEcuName}' gateways onto '{strNetworkId}', the network it is itself on")]
    GatewayOntoOwnNetwork {
        /// The ECU.
        strEcuName: String,
        /// The network it both sits on and claims to forward onto.
        strNetworkId: String,
    },

    /// Two ECUs both claim to forward onto one network, so there is no single path to it.
    #[error("network '{strNetworkId}' is gatewayed by both '{strFirstEcu}' and '{strSecondEcu}'; a network is reached through one gateway")]
    NetworkHasTwoGateways {
        /// The contested network.
        strNetworkId: String,
        /// The ECU that claimed it first.
        strFirstEcu: String,
        /// The ECU that claimed it second.
        strSecondEcu: String,
    },

    /// Following the gateways leads back to where it started, so nothing is reachable.
    #[error(
        "the gateways form a loop: {strCycle}. Every network would be reached only through itself"
    )]
    GatewayCycle {
        /// The loop, written as a chain of network ids.
        strCycle: String,
    },
}

/// How a tester reaches one ECU: through how many gateways, and which ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticPath {
    /// Gateway ECUs between the tester and this one, nearest the tester first. Empty for an
    /// ECU the tester can address directly.
    pub m_vecGatewayEcuNames: Vec<String>,
    /// How many gateways the request crosses. `0` means the ECU is on an entry-point link.
    pub m_uHopCount: usize,
    /// False when no chain of gateways connects an entry point to this ECU's network. Such an
    /// ECU is in the model but nothing could talk to it, which is worth showing rather than
    /// hiding.
    pub m_bIsReachable: bool,
}

impl Vehicle {
    /// Check that the declared wiring describes a vehicle that could exist.
    ///
    /// Called before a vehicle is accepted from any of the three sources, so a bad file, a bad
    /// API call and a bad hand-built model all fail the same way and at the same moment,
    /// rather than producing a diagram that quietly makes no sense.
    pub fn ValidateTopology(&self) -> Result<(), TopologyError> {
        for ecu in &self.m_vecEcus {
            if let Some(strNetworkId) = &ecu.m_optStrNetworkId {
                if self.FindNetwork(strNetworkId).is_none() {
                    return Err(TopologyError::UnknownNetwork {
                        strEcuName: ecu.m_strName.clone(),
                        strNetworkId: strNetworkId.clone(),
                    });
                }
            }

            for strBehindId in &ecu.m_vecGatewayForNetworkIds {
                if self.FindNetwork(strBehindId).is_none() {
                    return Err(TopologyError::UnknownNetwork {
                        strEcuName: ecu.m_strName.clone(),
                        strNetworkId: strBehindId.clone(),
                    });
                }
                if ecu.m_optStrNetworkId.as_deref() == Some(strBehindId.as_str()) {
                    return Err(TopologyError::GatewayOntoOwnNetwork {
                        strEcuName: ecu.m_strName.clone(),
                        strNetworkId: strBehindId.clone(),
                    });
                }
            }
        }

        self.RejectSharedGateways()?;
        self.RejectGatewayCycles()
    }

    /// Refuse two ECUs forwarding onto the same network.
    ///
    /// Real vehicles do sometimes have redundant paths, but the model has one path per network
    /// and a diagram drawn from an ambiguous one would simply pick whichever came first.
    fn RejectSharedGateways(&self) -> Result<(), TopologyError> {
        let mut mapClaims: BTreeMap<&str, &str> = BTreeMap::new();

        for ecu in &self.m_vecEcus {
            for strBehindId in &ecu.m_vecGatewayForNetworkIds {
                match mapClaims.get(strBehindId.as_str()) {
                    Some(strFirstEcu) => {
                        return Err(TopologyError::NetworkHasTwoGateways {
                            strNetworkId: strBehindId.clone(),
                            strFirstEcu: (*strFirstEcu).to_string(),
                            strSecondEcu: ecu.m_strName.clone(),
                        });
                    }
                    None => {
                        mapClaims.insert(strBehindId.as_str(), ecu.m_strName.as_str());
                    }
                }
            }
        }
        Ok(())
    }

    /// Refuse wiring where following the gateways never reaches the tester.
    ///
    /// Walk upward from every network: a network is reached through its gateway ECU, which
    /// sits on another network, and so on. Meeting a network twice on one walk is a loop.
    fn RejectGatewayCycles(&self) -> Result<(), TopologyError> {
        for network in &self.m_vecNetworks {
            let mut vecWalked: Vec<&str> = vec![network.m_strId.as_str()];
            let mut strCurrentId: &str = network.m_strId.as_str();

            // Nothing forwarding onto the current network ends the walk, loop-free.
            while let Some(gateway) = self.FindGatewayForNetwork(strCurrentId) {
                let strUpstreamId = match &gateway.m_optStrNetworkId {
                    Some(strUpstreamId) => strUpstreamId.as_str(),
                    // The gateway is on no declared network, so the walk cannot continue.
                    None => break,
                };

                if vecWalked.contains(&strUpstreamId) {
                    vecWalked.push(strUpstreamId);
                    return Err(TopologyError::GatewayCycle {
                        strCycle: vecWalked.join(" → "),
                    });
                }
                vecWalked.push(strUpstreamId);
                strCurrentId = strUpstreamId;
            }
        }
        Ok(())
    }

    /// Decide which links a tester can attach to, when nobody has said.
    ///
    /// A file or a user that never marks an entry point still deserves a working diagram, so
    /// every network nothing gateways onto becomes one: those are exactly the links that are
    /// not behind anything. Called after any change to networks or gateways. An explicit
    /// choice is left alone — if the author marked even one entry point, that is the answer.
    pub fn NormalizeEntryPoints(&mut self) {
        let bHasExplicitChoice = self
            .m_vecNetworks
            .iter()
            .any(|network| network.m_bIsDiagnosticEntryPoint);
        if bHasExplicitChoice {
            return;
        }

        let mut setGatewayedIds: BTreeSet<&str> = BTreeSet::new();
        for ecu in &self.m_vecEcus {
            for strBehindId in &ecu.m_vecGatewayForNetworkIds {
                setGatewayedIds.insert(strBehindId.as_str());
            }
        }

        let vecEntryPointIds: Vec<String> = self
            .m_vecNetworks
            .iter()
            .filter(|network| !setGatewayedIds.contains(network.m_strId.as_str()))
            .map(|network| network.m_strId.clone())
            .collect();

        for network in &mut self.m_vecNetworks {
            network.m_bIsDiagnosticEntryPoint = vecEntryPointIds.contains(&network.m_strId);
        }
    }

    /// How many gateways a tester crosses to reach each network, keyed by network id.
    ///
    /// A network missing from the result is one no chain of gateways connects to an entry
    /// point. Breadth-first from the entry points, so the answer is the shortest path.
    pub fn NetworkDepths(&self) -> BTreeMap<String, usize> {
        let mut mapDepths: BTreeMap<String, usize> = BTreeMap::new();
        let mut queueFrontier: VecDeque<(String, usize)> = VecDeque::new();

        for network in &self.m_vecNetworks {
            if network.m_bIsDiagnosticEntryPoint {
                mapDepths.insert(network.m_strId.clone(), 0);
                queueFrontier.push_back((network.m_strId.clone(), 0));
            }
        }

        while let Some((strNetworkId, uDepth)) = queueFrontier.pop_front() {
            // Every ECU on this network may forward onto further ones, which are one hop
            // deeper than the network the gateway itself sits on.
            for ecu in &self.m_vecEcus {
                if ecu.m_optStrNetworkId.as_deref() != Some(strNetworkId.as_str()) {
                    continue;
                }
                for strBehindId in &ecu.m_vecGatewayForNetworkIds {
                    if mapDepths.contains_key(strBehindId) {
                        continue;
                    }
                    mapDepths.insert(strBehindId.clone(), uDepth + 1);
                    queueFrontier.push_back((strBehindId.clone(), uDepth + 1));
                }
            }
        }
        mapDepths
    }

    /// The chain of gateways a tester goes through to reach one ECU.
    ///
    /// An ECU on no declared network is treated as directly reachable: "nobody said how it is
    /// wired" must not become "it is unreachable", or every log-reconstructed vehicle would
    /// render as a set of unreachable ECUs.
    pub fn DiagnosticPathTo(&self, ecu: &Ecu) -> DiagnosticPath {
        let strNetworkId = match &ecu.m_optStrNetworkId {
            Some(strNetworkId) => strNetworkId.clone(),
            None => {
                return DiagnosticPath {
                    m_vecGatewayEcuNames: Vec::new(),
                    m_uHopCount: 0,
                    m_bIsReachable: true,
                }
            }
        };

        let mut vecGatewayNames: Vec<String> = Vec::new();
        let mut strCurrentId = strNetworkId;

        // Walk from the ECU's own network up towards a tester, collecting the gateways
        // crossed. The cycle check in `ValidateTopology` is what makes this terminate; the
        // hop limit is a belt-and-braces stop for a model that never went through it.
        let uHopLimit = self.m_vecNetworks.len() + 1;
        for _ in 0..uHopLimit {
            let bIsEntryPoint = self
                .FindNetwork(&strCurrentId)
                .map(|network| network.m_bIsDiagnosticEntryPoint)
                .unwrap_or(false);
            if bIsEntryPoint {
                // Collected nearest-the-ECU first; a reader wants tester-first.
                vecGatewayNames.reverse();
                return DiagnosticPath {
                    m_uHopCount: vecGatewayNames.len(),
                    m_vecGatewayEcuNames: vecGatewayNames,
                    m_bIsReachable: true,
                };
            }

            let gateway = match self.FindGatewayForNetwork(&strCurrentId) {
                Some(gateway) => gateway,
                None => break,
            };
            vecGatewayNames.push(gateway.m_strName.clone());
            strCurrentId = match &gateway.m_optStrNetworkId {
                Some(strUpstreamId) => strUpstreamId.clone(),
                None => break,
            };
        }

        DiagnosticPath {
            m_uHopCount: vecGatewayNames.len(),
            m_vecGatewayEcuNames: Vec::new(),
            m_bIsReachable: false,
        }
    }
}
