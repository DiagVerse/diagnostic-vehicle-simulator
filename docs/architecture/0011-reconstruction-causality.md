# ADR 0011 — A PDU is an interval, not an instant

Status: accepted

## Context

Two defects were carried on the deferred list since the MVP-1 CAN/UDS review, both in the
offline reconstruction path. Looking at them together showed they were the same defect.

`IsoTpMessage` carried one timestamp: the frame that *started* the message. That is fine for a
single frame, which begins and ends at the same instant, but a multi-frame PDU occupies an
interval — a `0x36` TransferData or a long `0x2E` write can be on the bus for a noticeable time.
With only a start time recorded, there was no way to express the one hard physical constraint in
a diagnostic exchange: **an ECU cannot begin answering a request it has not finished receiving.**

Correlation therefore paired a response with any outstanding request whose service identifier
echoed, regardless of whether that request had even finished transmitting. A response arriving
mid-request was attributed to that request, and reconstruction recorded the result as *observed*
ECU behaviour — a confident, specific, wrong answer, which is precisely what the confidence model
exists to prevent (README §7).

Separately, `AppendConsecutive` ignored the sequence number in each consecutive frame. A capture
with a dropped or duplicated frame reassembled into a PDU of the right length and the wrong
contents. The live receiver added in MVP-3 (`isotp/src/rx.rs`) had always refused these; the
offline path had not caught up.

## Decision

**Record both ends of a message.** `IsoTpMessage` now carries `m_f64StartedAtSec` and
`m_f64CompletedAtSec`. The rename from `m_f64TimestampSec` is deliberate: the old name did not
say *which* end it was, and that ambiguity is what allowed the bug to be written in the first
place.

**Order by start, constrain by completion.** These are two different jobs and were conflated.

- PDUs are sorted by **start** time, because that is the order a bus observer saw them begin.
  Sorting by completion was the obvious-looking fix and is wrong: a long response can still be
  in flight when the tester sends its next request, and completion order would let that later
  request evict — via `RememberRequest` — the very request the response answers.
- Pairing is then filtered by **completion**: `CouldHaveAnswered` requires the response to have
  started at or after the moment its candidate request finished. Equality is allowed because
  capture timestamps are not perfectly precise; the check exists to reject a response that began
  while whole frames of the request were still to come, not to arbitrate rounding.

**Validate consecutive-frame sequence numbers offline, matching the live receiver.** Out of
sequence, or duplicated, abandons the message and logs a warning naming the CAN id, the expected
and received numbers, and how many bytes had been assembled. Abandoning is the same choice
`rx.rs` makes, for the same reason: a partial PDU that looks whole is worse than no PDU.

The counter wraps 15 → 0, and there is a test for a 118-byte message needing sixteen consecutive
frames — a strict check that rejected a valid long message would simply be a new bug.

## Verification

Each fix was proved by reverting it and watching the new test fail:

- `a_consecutive_frame_out_of_sequence_abandons_the_message` and
  `a_duplicated_consecutive_frame_abandons_the_message` fail without the sequence check.
- `a_response_that_began_before_its_request_finished_is_not_paired_with_it` fails without the
  causality filter, reconstructing **two** ECUs instead of one and claiming DID 0xF190 was
  answered when it never was.

A real vehicle capture reconstructs identically before and after — the same three ECUs with the
same DIDs, DTCs and services, and no sequence warnings — confirming the change does not disturb
well-formed logs.

## Consequences

- A corrupt or lossy capture now yields *fewer* facts rather than wrong ones. That is the right
  trade for this project: a partial vehicle is the documented goal, a confidently wrong one is
  not.
- The warning names the CAN id and the byte count, so a user whose log reconstructs thinner than
  expected can see it was the capture and not the parser.
- `IsoTpMessage`'s field rename touches only `reconstruct`, the sole consumer.
- The live and offline ISO-TP paths now agree about what a valid message is. They remain separate
  implementations — `rx.rs` is a bus participant that generates flow control, `lib.rs` is an
  observer that skips it — but they no longer disagree about correctness.
