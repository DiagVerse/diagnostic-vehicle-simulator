//! Executing a response plan against a clock.
//!
//! A plan says *what* goes on the wire and *when* (ADR 0005); something has to actually wait
//! and then hand the bytes to a transport. That waiting is identical whether the transport is
//! an HTTP response being assembled or a serial port carrying CAN frames, so it lives here
//! rather than in either of them.
//!
//! This is the one place in the simulation layer that touches time. Everything else — routing,
//! scheduling, overrides — is pure arithmetic and tests without a clock.

#![allow(non_snake_case, non_upper_case_globals)]

use ecu::schedule::ScheduledResponse;
use tokio::time::{sleep_until, Duration, Instant};

use crate::RoutedResponse;

/// One message as it actually went out.
#[derive(Debug, Clone)]
pub struct EmittedFrame<'a> {
    /// Which answer in the batch this belongs to — the index into the responses given.
    pub m_uResponseIndex: usize,
    /// The scheduled message: its bytes, its planned offset, and whether it is a pending.
    pub m_step: &'a ScheduledResponse,
    /// Milliseconds actually elapsed when it was emitted.
    pub m_u64ActualMs: u64,
}

/// Wait out every answer's schedule and hand each message to `fnOnFrame` as it comes due.
///
/// All offsets are measured from the same instant — the completion of request reception — so
/// several ECUs answering one broadcast are merged into a single timeline and emitted in the
/// order a real bus would carry them.
///
/// The deadline for each step is absolute rather than a gap from the previous one, so the
/// small per-iteration overhead cannot accumulate across a long ResponsePending sequence.
pub async fn ExecutePlans(
    vecResponses: &[RoutedResponse],
    // `Send` because the caller may be an async HTTP handler, whose future has to be, and a
    // sink that is not would quietly make the whole handler unusable.
    fnOnFrame: &mut (dyn FnMut(EmittedFrame<'_>) + Send),
) {
    let mut vecTimeline: Vec<(usize, &ScheduledResponse)> = Vec::new();
    for (uResponseIndex, response) in vecResponses.iter().enumerate() {
        for step in &response.m_plan.m_vecSteps {
            vecTimeline.push((uResponseIndex, step));
        }
    }
    vecTimeline.sort_by_key(|(_, step)| step.m_u32AtMs);

    let baseline = Instant::now();
    for (uResponseIndex, step) in vecTimeline {
        sleep_until(baseline + Duration::from_millis(step.m_u32AtMs as u64)).await;

        fnOnFrame(EmittedFrame {
            m_uResponseIndex: uResponseIndex,
            m_step: step,
            m_u64ActualMs: baseline.elapsed().as_millis() as u64,
        });
    }
}
