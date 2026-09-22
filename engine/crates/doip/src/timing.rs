//! The DoIP timing parameters (ISO 13400-2:2019 Table 12, clause 7.7).
//!
//! Named constants rather than numbers at the call site, because every one of them is a figure
//! from the standard and a reader needs to be able to check it against the table.

use std::time::Duration;

/// `A_DoIP_Ctrl` — the deadline for answering a UDP request. A client gives up after this, so
/// entity status and power mode answers must be inside it (REQ 8.DoIP-118 / 8.DoIP-121 APP).
pub const c_ctrlTimeout: Duration = Duration::from_secs(2);

/// `A_DoIP_Announce_Wait` maximum — the random delay before announcing, and before answering a
/// vehicle identification request.
///
/// Randomised per entity, and the randomisation is the point: it de-bursts many entities
/// answering one broadcast. A fixed value, or one drawn once and shared, defeats it.
pub const c_announceWaitMax: Duration = Duration::from_millis(500);

/// `A_DoIP_Announce_Interval` — the gap between the announcements sent at power-up.
pub const c_announceInterval: Duration = Duration::from_millis(500);

/// `A_DoIP_Announce_Num` — how many announcements go out after an address is configured.
///
/// Three, spaced 500 ms, because UDP has no delivery guarantee and the vehicle only gets one
/// chance to be noticed. This is loss compensation, not a retry storm.
pub const c_uAnnounceCount: usize = 3;

/// `A_DoIP_Diagnostic_Message` performance target — how quickly a diagnostic message should be
/// acknowledged, measured from its last byte.
pub const c_diagnosticAckTarget: Duration = Duration::from_millis(50);

/// `T_TCP_General_Inactivity` — how long a socket may sit with nothing sent *or received* on it
/// before the entity closes it (REQ 3.DoIP-080 / 3.DoIP-082 NL).
pub const c_generalInactivity: Duration = Duration::from_secs(300);

/// `T_TCP_Initial_Inactivity` — how long a freshly established socket has to produce a valid
/// routing activation request before the entity closes it (REQ 3.DoIP-084 / 3.DoIP-086 NL).
///
/// A measure against connections that send nothing, or nothing valid.
pub const c_initialInactivity: Duration = Duration::from_secs(2);

/// `T_TCP_Alive_Check` — how long to wait for an alive check response before treating the
/// socket as dead (REQ 3.DoIP-092 NL).
pub const c_aliveCheckTimeout: Duration = Duration::from_millis(500);

/// Draw one `A_DoIP_Announce_Wait` delay, in milliseconds.
///
/// REQ 8.DoIP-051 APP requires the vehicle identification *response* to be delayed by this
/// parameter, and the standard's own note says why: without it, every entity on the network
/// answers one broadcast in the same instant and the resulting UDP burst drops packets on the
/// way back to the tester. The delay only works if entities pick different values.
///
/// Not a cryptographic draw, and it does not need to be — nothing here is a secret. What is
/// required is spread, and the system clock's nanoseconds differ between two entities on two
/// machines and between two requests to one entity, which is exactly that.
pub fn DrawAnnounceWaitMs() -> u32 {
    let u32MaxMs = c_announceWaitMax.as_millis() as u32;
    let u32Nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);

    u32Nanos % (u32MaxMs + 1)
}
