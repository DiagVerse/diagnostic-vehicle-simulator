//! Response scheduling — turning one request into the sequence of messages the ECU puts on
//! the wire, and when.
//!
//! A UDS server does not always answer once. When it needs longer than P2Server_max it must
//! first send NRC 0x78 ResponsePending, may repeat that, and must eventually send a final
//! response (ISO 14229-1 Annex A.1). Modelling that as a **plan** — a list of byte strings
//! with millisecond offsets — keeps every timing *decision* pure arithmetic, so it can be
//! unit-tested without a clock. Only the transport that executes a plan ever sleeps.
//!
//! Offsets are measured from the moment the request was completely received: `T_Data.ind`,
//! which on CAN is the **last** frame of the request, not the first (ISO 14229-2 clause 7.1.1,
//! Figure 4). The offset of a step is when its **first** frame goes out.

#![allow(non_snake_case, non_upper_case_globals)]

use core_domain::model::EcuTiming;

/// First byte of a UDS negative response.
const c_byNegativeResponseSid: u8 = 0x7F;
/// NRC 0x78 — requestCorrectlyReceived-ResponsePending (ISO 14229-1 Annex A.1).
const c_byNrcResponsePending: u8 = 0x78;

/// ISO 14229-2 Table 4 footnote b: consecutive ResponsePending messages must be at least
/// 0.3 x P2*Server_max apart, so a slow server does not flood the link.
const c_u32AntiFloodNumerator: u32 = 3;
const c_u32AntiFloodDenominator: u32 = 10;

/// Smallest spacing used when the operator forces ResponsePending messages with (nearly) no
/// delay, so the schedule still has strictly increasing offsets. Not an ISO value.
const c_u32MinPendingStepMs: u32 = 10;

/// One message the ECU puts on the wire, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledResponse {
    /// Offset in milliseconds from the completion of request reception.
    pub m_u32AtMs: u32,
    /// The UDS bytes to transmit.
    pub m_vecBytes: Vec<u8>,
    /// True for `7F <sid> 78`; false for the final response.
    pub m_bIsResponsePending: bool,
}

/// The complete answer to one request, spread over time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponsePlan {
    /// Ascending by offset. Empty when nothing goes on the wire at all.
    pub m_vecSteps: Vec<ScheduledResponse>,
    /// True when fault injection withheld the final response. Distinct from a suppressed
    /// positive response: both are silence, this records why.
    pub m_bIsFinalResponseDropped: bool,
    /// False when the schedule knowingly breaks an ISO 14229-2 timing rule. Such a plan is
    /// still executed — refusing to inject a fault would defeat the purpose — but it is
    /// flagged rather than presented as conformant.
    pub m_bIsIsoConformant: bool,
    /// One line per broken rule, with the numbers, for the UI and the log.
    pub m_vecConformanceWarnings: Vec<String>,
    /// How many ResponsePending messages this plan contains.
    pub m_u8ResponsePendingCount: u8,
}

impl ResponsePlan {
    /// The offset of the final response, or `None` when none is sent.
    pub fn FinalAtMs(&self) -> Option<u32> {
        self.m_vecSteps
            .iter()
            .find(|step| !step.m_bIsResponsePending)
            .map(|step| step.m_u32AtMs)
    }

    /// The final response bytes, or an empty slice when none is sent.
    pub fn FinalResponse(&self) -> &[u8] {
        self.m_vecSteps
            .iter()
            .find(|step| !step.m_bIsResponsePending)
            .map(|step| step.m_vecBytes.as_slice())
            .unwrap_or(&[])
    }
}

/// How many ResponsePending messages this request needs.
///
/// Two reasons to send them, and the larger wins: the configured delay pushes the answer past
/// P2Server_max and the standard then *requires* them, or the operator forced them to
/// demonstrate the path without waiting.
///
/// The caller is responsible for the cases where NRC 0x78 is not allowed at all
/// (unsupported service, or `P4 == P2` — ISO 14229-2 clause 7.1.1) and must pass those
/// straight to a plan with no pending messages.
pub fn ResolveResponsePendingCount(timing: &EcuTiming) -> u8 {
    let u32Required = RequiredResponsePendingCount(timing);

    let u32Forced = if timing.m_bForceResponsePending {
        timing.m_u8ForcedResponsePendingCount as u32
    } else {
        0
    };

    let u32Count = u32Required.max(u32Forced);
    u32Count.min(u8::MAX as u32) as u8
}

/// The smallest number of ResponsePending messages that keeps every gap inside its budget:
/// the first response must start within P2, and each later one within P2* of the previous.
fn RequiredResponsePendingCount(timing: &EcuTiming) -> u32 {
    if timing.m_u32ResponseDelayMs <= timing.m_u32P2ServerMaxMs {
        return 0;
    }
    if timing.m_u32P2StarServerMaxMs == 0 {
        // No enhanced budget exists, so no number of pending messages would make the delay
        // conformant. Say zero and let the conformance check report the P2 violation.
        return 0;
    }

    let u32Overrun = timing.m_u32ResponseDelayMs - timing.m_u32P2ServerMaxMs;
    u32Overrun.div_ceil(timing.m_u32P2StarServerMaxMs)
}

/// Build the plan for one request.
///
/// `vecFinalResponse` is what the protocol handler produced; empty means the ECU deliberately
/// sent nothing (suppressPosRspMsgIndicationBit). `u8PendingCount` comes from
/// [`ResolveResponsePendingCount`], already gated by the caller.
pub fn BuildResponsePlan(
    timing: &EcuTiming,
    byRequestSid: u8,
    vecFinalResponse: &[u8],
    u8PendingCount: u8,
) -> ResponsePlan {
    let u32FinalAtMs = FinalInstantMs(timing, u8PendingCount);
    let vecPendingOffsets = PendingOffsetsMs(timing, u8PendingCount, u32FinalAtMs);

    let mut vecSteps: Vec<ScheduledResponse> = vecPendingOffsets
        .iter()
        .map(|u32AtMs| ScheduledResponse {
            m_u32AtMs: *u32AtMs,
            m_vecBytes: vec![
                c_byNegativeResponseSid,
                byRequestSid,
                c_byNrcResponsePending,
            ],
            m_bIsResponsePending: true,
        })
        .collect();

    // A suppressed response has no bytes to send, so there is no final step to schedule.
    let bHasFinalResponse = !vecFinalResponse.is_empty() && !timing.m_bDropFinalResponse;
    if bHasFinalResponse {
        vecSteps.push(ScheduledResponse {
            m_u32AtMs: u32FinalAtMs,
            m_vecBytes: vecFinalResponse.to_vec(),
            m_bIsResponsePending: false,
        });
    }

    let bIsFinalResponseDropped = timing.m_bDropFinalResponse && !vecFinalResponse.is_empty();

    let vecWarnings = CollectConformanceWarnings(
        timing,
        &vecPendingOffsets,
        u32FinalAtMs,
        bIsFinalResponseDropped,
    );

    ResponsePlan {
        m_vecSteps: vecSteps,
        m_bIsFinalResponseDropped: bIsFinalResponseDropped,
        m_bIsIsoConformant: vecWarnings.is_empty(),
        m_vecConformanceWarnings: vecWarnings,
        m_u8ResponsePendingCount: u8PendingCount,
    }
}

/// When the final response starts.
///
/// Normally the configured delay. The lower bound only matters in the degenerate case where
/// the operator forces several ResponsePending messages with no delay at all; without it the
/// schedule would not be strictly increasing.
fn FinalInstantMs(timing: &EcuTiming, u8PendingCount: u8) -> u32 {
    if u8PendingCount == 0 {
        // Nothing has to be fitted in beforehand, so the answer goes out exactly when asked —
        // immediately, when no delay is configured.
        return timing.m_u32ResponseDelayMs;
    }

    let u32MinimumSpan = (u8PendingCount as u32 + 1) * c_u32MinPendingStepMs;
    timing.m_u32ResponseDelayMs.max(u32MinimumSpan)
}

/// Where the ResponsePending messages fall, spread evenly between the first one and the final
/// response. The first is never later than P2Server_max — that is the deadline it exists to
/// meet.
fn PendingOffsetsMs(timing: &EcuTiming, u8PendingCount: u8, u32FinalAtMs: u32) -> Vec<u32> {
    if u8PendingCount == 0 {
        return Vec::new();
    }

    let u32Count = u8PendingCount as u32;
    let u32FirstAtMs = timing.m_u32P2ServerMaxMs.min(u32FinalAtMs / (u32Count + 1));
    let u32StepMs = (u32FinalAtMs - u32FirstAtMs) / u32Count;

    (0..u32Count)
        .map(|u32Index| u32FirstAtMs + u32Index * u32StepMs)
        .collect()
}

/// Check the schedule against the ISO 14229-2 timing rules, returning one message per breach.
///
/// A breach is reported, not corrected: the operator may be deliberately simulating a server
/// that floods the link or never answers, and the engine's job is to be honest about what it
/// is doing rather than to quietly refuse.
fn CollectConformanceWarnings(
    timing: &EcuTiming,
    vecPendingOffsets: &[u32],
    u32FinalAtMs: u32,
    bIsFinalResponseDropped: bool,
) -> Vec<String> {
    let mut vecWarnings = Vec::new();

    if bIsFinalResponseDropped {
        vecWarnings.push(
            "no final response is sent; ISO 14229-1 Annex A.1 requires one after a ResponsePending, and P4Server_max would be missed in any case"
                .to_string(),
        );
    }

    if vecPendingOffsets.is_empty() {
        if u32FinalAtMs > timing.m_u32P2ServerMaxMs {
            vecWarnings.push(format!(
                "the response starts at {u32FinalAtMs} ms with no ResponsePending, exceeding P2Server_max of {} ms",
                timing.m_u32P2ServerMaxMs
            ));
        }
    } else {
        CheckPendingGaps(timing, vecPendingOffsets, u32FinalAtMs, &mut vecWarnings);
    }

    if !bIsFinalResponseDropped && u32FinalAtMs > timing.m_u32P4ServerMaxMs {
        vecWarnings.push(format!(
            "the final response starts at {u32FinalAtMs} ms, exceeding P4Server_max of {} ms",
            timing.m_u32P4ServerMaxMs
        ));
    }

    vecWarnings
}

/// The two rules that govern a ResponsePending sequence: no gap may exceed P2*Server_max, and
/// consecutive pending messages must not be closer than the anti-flood floor.
fn CheckPendingGaps(
    timing: &EcuTiming,
    vecPendingOffsets: &[u32],
    u32FinalAtMs: u32,
    vecWarnings: &mut Vec<String>,
) {
    let u32AntiFloodFloorMs =
        (timing.m_u32P2StarServerMaxMs * c_u32AntiFloodNumerator) / c_u32AntiFloodDenominator;

    for uIndex in 1..vecPendingOffsets.len() {
        let u32GapMs = vecPendingOffsets[uIndex] - vecPendingOffsets[uIndex - 1];
        if u32GapMs < u32AntiFloodFloorMs {
            vecWarnings.push(format!(
                "consecutive ResponsePending messages are {u32GapMs} ms apart; ISO 14229-2 Table 4 requires at least 0.3 x P2*Server_max, which is {u32AntiFloodFloorMs} ms"
            ));
            break;
        }
    }

    // Every gap after a ResponsePending is budgeted by P2*, including the one before the final
    // response.
    let mut u32PreviousMs = vecPendingOffsets[0];
    for u32AtMs in vecPendingOffsets.iter().skip(1).chain([&u32FinalAtMs]) {
        let u32GapMs = u32AtMs - u32PreviousMs;
        if u32GapMs > timing.m_u32P2StarServerMaxMs {
            vecWarnings.push(format!(
                "a gap of {u32GapMs} ms exceeds P2*Server_max of {} ms",
                timing.m_u32P2StarServerMaxMs
            ));
            break;
        }
        u32PreviousMs = *u32AtMs;
    }

    if vecPendingOffsets[0] > timing.m_u32P2ServerMaxMs {
        vecWarnings.push(format!(
            "the first ResponsePending starts at {} ms, exceeding P2Server_max of {} ms",
            vecPendingOffsets[0], timing.m_u32P2ServerMaxMs
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response to `22 F1 90` used throughout: `62 F1 90` plus a VIN.
    ///
    /// A VIN is 17 characters (ISO 3779), so this PDU is 20 bytes — long enough to need
    /// ISO-TP segmentation, which is what a realistic ReadDataByIdentifier(0xF190) looks like.
    fn ReadVinResponse() -> Vec<u8> {
        let mut vecResponse = vec![0x62, 0xF1, 0x90];
        vecResponse.extend_from_slice(c_strSampleVin.as_bytes());
        vecResponse
    }

    /// A syntactically valid 17-character VIN (ISO 3779).
    const c_strSampleVin: &str = "1HGCM82633A004352";

    #[test]
    fn the_sample_vin_is_seventeen_characters() {
        // ISO 3779 fixes the VIN at 17 characters; a fixture that is not stops the DID being
        // representative of what an ECU really returns.
        assert_eq!(c_strSampleVin.len(), 17);
        assert_eq!(ReadVinResponse().len(), 20);
    }

    /// Build a plan the way `VirtualEcu` does, for a supported service.
    fn PlanFor(timing: &EcuTiming, byRequestSid: u8, vecFinalResponse: &[u8]) -> ResponsePlan {
        let u8PendingCount = ResolveResponsePendingCount(timing);
        BuildResponsePlan(timing, byRequestSid, vecFinalResponse, u8PendingCount)
    }

    fn OffsetsOf(plan: &ResponsePlan) -> Vec<u32> {
        plan.m_vecSteps.iter().map(|step| step.m_u32AtMs).collect()
    }

    #[test]
    fn an_answer_inside_p2_is_a_single_immediate_step() {
        let timing = EcuTiming::default();
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(plan.m_vecSteps.len(), 1);
        assert_eq!(plan.m_vecSteps[0].m_u32AtMs, 0);
        assert!(!plan.m_vecSteps[0].m_bIsResponsePending);
        assert_eq!(plan.m_u8ResponsePendingCount, 0);
        assert!(plan.m_bIsIsoConformant);
        assert!(plan.m_vecConformanceWarnings.is_empty());
    }

    #[test]
    fn a_delay_within_p2_still_needs_no_response_pending() {
        // The boundary matters: P2Server_max is the deadline to *start* the response, so a
        // delay equal to it is still in time.
        for u32DelayMs in [40, 50] {
            let timing = EcuTiming {
                m_u32ResponseDelayMs: u32DelayMs,
                ..EcuTiming::default()
            };
            let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

            assert_eq!(plan.m_u8ResponsePendingCount, 0, "delay {u32DelayMs} ms");
            assert_eq!(OffsetsOf(&plan), vec![u32DelayMs]);
            assert!(plan.m_bIsIsoConformant);
        }

        // One millisecond later, the standard requires a ResponsePending.
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 51,
            ..EcuTiming::default()
        };
        assert_eq!(
            PlanFor(&timing, 0x22, &ReadVinResponse()).m_u8ResponsePendingCount,
            1
        );
    }

    #[test]
    fn a_delay_beyond_p2_inserts_the_required_response_pending() {
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 200,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(plan.m_u8ResponsePendingCount, 1);
        assert_eq!(OffsetsOf(&plan), vec![50, 200]);
        // The SID is echoed in the negative response (ISO 14229-1 Table 3).
        assert_eq!(plan.m_vecSteps[0].m_vecBytes, vec![0x7F, 0x22, 0x78]);
        assert!(plan.m_vecSteps[0].m_bIsResponsePending);
        assert_eq!(plan.FinalAtMs(), Some(200));
        assert!(
            plan.m_bIsIsoConformant,
            "{:?}",
            plan.m_vecConformanceWarnings
        );
    }

    #[test]
    fn forcing_a_response_pending_with_no_delay_still_produces_an_ordered_schedule() {
        let timing = EcuTiming {
            m_bForceResponsePending: true,
            m_u8ForcedResponsePendingCount: 1,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(OffsetsOf(&plan), vec![10, 20]);
        assert_eq!(plan.m_u8ResponsePendingCount, 1);
        assert!(plan.m_bIsIsoConformant);
    }

    #[test]
    fn a_long_delay_derives_the_number_of_response_pendings_from_p2_star() {
        // 12 s with a 5 s enhanced budget needs three: 50 + 3 x 5000 covers it, 50 + 2 x 5000
        // does not.
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 12_000,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(plan.m_u8ResponsePendingCount, 3);
        assert_eq!(OffsetsOf(&plan), vec![50, 4033, 8016, 12_000]);
        assert!(
            plan.m_bIsIsoConformant,
            "{:?}",
            plan.m_vecConformanceWarnings
        );
    }

    #[test]
    fn flooding_response_pendings_is_executed_but_reported_as_non_conformant() {
        // Three pendings inside 200 ms puts them 50 ms apart, far below the 1500 ms floor
        // ISO 14229-2 Table 4 sets at 0.3 x P2*Server_max.
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 200,
            m_bForceResponsePending: true,
            m_u8ForcedResponsePendingCount: 3,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(OffsetsOf(&plan), vec![50, 100, 150, 200]);
        assert_eq!(plan.m_u8ResponsePendingCount, 3);
        assert!(!plan.m_bIsIsoConformant);
        assert!(
            plan.m_vecConformanceWarnings
                .iter()
                .any(|strWarning| strWarning.contains("1500")),
            "the anti-flood floor should be named: {:?}",
            plan.m_vecConformanceWarnings
        );
        // The messages still go out — a fault injector that refuses to inject is useless.
        assert_eq!(plan.m_vecSteps.len(), 4);
    }

    #[test]
    fn dropping_the_final_response_leaves_only_the_pending_messages() {
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 200,
            m_bForceResponsePending: true,
            m_u8ForcedResponsePendingCount: 1,
            m_bDropFinalResponse: true,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert_eq!(plan.m_vecSteps.len(), 1);
        assert!(plan.m_vecSteps[0].m_bIsResponsePending);
        assert!(plan.m_bIsFinalResponseDropped);
        assert!(plan.FinalResponse().is_empty());
        assert!(!plan.m_bIsIsoConformant);
    }

    #[test]
    fn a_suppressed_response_schedules_nothing_at_all() {
        let timing = EcuTiming::default();
        let plan = PlanFor(&timing, 0x3E, &[]);

        assert!(plan.m_vecSteps.is_empty());
        // Nothing was withheld by fault injection; the ECU was asked to stay quiet.
        assert!(!plan.m_bIsFinalResponseDropped);
        assert!(plan.m_bIsIsoConformant);
    }

    #[test]
    fn a_final_response_beyond_p4_is_reported() {
        let timing = EcuTiming {
            m_u32ResponseDelayMs: 20_000,
            m_u32P4ServerMaxMs: 10_000,
            ..EcuTiming::default()
        };
        let plan = PlanFor(&timing, 0x22, &ReadVinResponse());

        assert!(!plan.m_bIsIsoConformant);
        assert!(
            plan.m_vecConformanceWarnings
                .iter()
                .any(|strWarning| strWarning.contains("P4Server_max")),
            "{:?}",
            plan.m_vecConformanceWarnings
        );
    }
}
