# ADR 0005 — User-controllable UDS server timing

Status: accepted (MVP-2)

## Context

An ECU that carries `EcuTiming { P2Server_max, P2*Server_max }` but always answers instantly is
not simulating timing — it is storing numbers. MVP-2 makes them effective: the operator sets a
delay in the browser, and the simulated ECU actually waits, actually sends NRC 0x78
ResponsePending, and actually answers afterwards.

The rules that make this non-trivial (ISO 14229-2 clause 7, ISO 14229-1 Annex A.1):

- **P2Server_max** is the deadline to *start* the response after the request has been
  **completely received** — on CAN, the last frame of the request, not the first.
- A server that needs longer must send NRC 0x78 first, and then has **P2\*Server_max** until it
  starts the next message. It may repeat, but consecutive 0x78s must be at least
  0.3 × P2\* apart (ISO 14229-2 Table 4 footnote b) so it does not flood the link.
- **P4Server_max** bounds the *final* response across the whole exchange.
- After a 0x78 a final response is **mandatory**, regardless of the
  suppressPosRspMsgIndicationBit and regardless of the functional-addressing suppression rules.
- P2/P2\* reach the tester only in the DiagnosticSessionControl positive response, as a
  four-byte sessionParameterRecord at 1 ms and 10 ms resolution (ISO 14229-1 Table 29).

## Decision

### 1. The ECU authors a plan; the transport executes it

An answer is modelled as a **`ResponsePlan`**: a list of byte strings with millisecond offsets
from the completion of request reception, plus whether the schedule is ISO-conformant.

| Layer | Responsibility | Sleeps? |
|---|---|---|
| `uds` plugin | Pure UDS semantics. **Unchanged.** | no |
| `core-domain::EcuTiming` | Timing values and their validation. | no |
| `ecu::VirtualEcu` + `ecu::schedule` | Decides how many 0x78s, their bytes and their offsets. | no |
| `simulation::SimulationService` | Routing; carries the plan through. | no |
| `api` (and the MVP-3 CAN bridge) | Executes the plan against a real clock. | **yes** |

Every timing *decision* is therefore pure arithmetic that unit-tests without a clock, and only
one place touches time. The plan is the artefact MVP-3's SLCAN bridge reuses: it drives its own
transmit state machine off the same offsets, segmenting each step through ISO-TP.

No `ResponseEmitter` trait yet — there is exactly one emitter. When there are two, the shared
shape will be known rather than guessed (CLAUDE.md §10).

### 2. Timing lives in the session layer, not the protocol plugin

P2/P2\*/P4 are ISO 14229-**2** parameters — the session layer — not service semantics. The UDS
plugin stays byte-for-byte unchanged, and `REcuSnapshot` / `RProtocolOutcome` are untouched:
they are `StableAbi` types, so adding a field would break every already-built plugin in
`plugins.d/`. This is the same split ADR 0004 made for functional NRC suppression.

Consequently the ECU **overlays** its live P2/P2\* onto the DiagnosticSessionControl response
after the plugin returns. A response too short to carry the record is reported, not completed:
those bytes belong to the plugin, and inventing four would hide the real problem.

*Future work:* when the plugin ABI is next revised deliberately, `REcuSnapshot` can become an
`abi_stable` prefix type, which allows appending fields compatibly.

### 3. The suppress bit is resolved before the handler runs

Once a 0x78 is planned the standard obliges a final response even with
suppressPosRspMsgIndicationBit set. A pure handler asked to suppress returns no bytes, and they
cannot be recovered without a second call or an ABI change — so the bit is cleared **in the
request handed to the handler**, and the decision is made first.

That is possible because both cases where 0x78 is forbidden are decidable without the handler:
an unsupported service (whose P4 always equals P2) and any server configured with `P4 == P2`,
both ISO 14229-2 clause 7.1.1.

The bit is only cleared for services whose sub-function actually carries it (ISO 14229-1
Table 11). Byte 1 of a ReadDataByIdentifier is the DID's high byte, so clearing bit 7 blindly
turned `22 F1 90` into a read of DID 0x7190 — a bug the tests caught, and the reason this is a
whitelist rather than a length check.

### 4. Reject values, execute behaviours

`EcuTiming::Validate` refuses anything that **cannot be put on the wire truthfully**: a P2 too
large for its two advertised bytes, a P2\* that is not a whole number of 10 ms units, P4 below
P2, forcing 0x78 on a server whose P4 equals P2. Rejections are reported with the offending
value, never clamped behind the operator's back.

It deliberately permits non-conformant *behaviour* — a server that floods the link with 0x78s,
one that answers past P4, one that never answers at all. A fault injector that refuses to inject
faults is useless. Such a schedule is executed and flagged on the plan with the rule it breaks
and the numbers involved, surfaced in the API and the UI, and logged at `warn`. That is
README §7 discipline: the engine never asserts conformance the behaviour does not support.

### 5. Broadcast suppression becomes conditional

ISO 14229-1 clause 7.5.1 keeps NRCs 0x11, 0x12, 0x31, 0x7E and 0x7F suppressed on a functionally
addressed request — 0x78 is **not** in that list and is always sent, because it means the
opposite of the five ("this request is for me and I am working on it").

But clause 7.5.5 and Annex A.1 add the inverse rule: once an ECU has answered a broadcast with a
0x78, its final negative response must be sent too. Having announced itself as present and busy,
going quiet would strand the tester until P2\* expires.

### 6. Concurrency

The routing decision is made under the simulation lock and returns a plan; the plan is executed
with the lock released, so one slow ECU does not block the whole simulation. The compiler
enforces that ordering: a `std::sync::MutexGuard` is `!Send` and cannot be held across an
`await`. **Do not "fix" this by swapping in a `tokio::sync::Mutex`.**

An ECU part-way through a ResponsePending sequence has told the tester it cannot receive another
request (Annex A.1), so a second one is refused with 409 rather than allowed to mutate its state
mid-answer. Single-step answers claim nothing, keeping the common case free of contention.

## Consequences

- A timing change applies to the **next** request; a plan already computed keeps the values it
  was built with, and the tester learns new P2/P2\* only at its next DiagnosticSessionControl —
  the API says so explicitly rather than leaving an operator to wonder.
- A diagnostic reset returns session and security to default but **keeps** operator-set timing:
  a reset clears state, not configuration.
- The `/ecu/*` dev endpoints keep the instantaneous path (`VirtualEcu::ProcessRequest`), where
  timing is not the subject.
- Routes are covered by in-process router tests. A route can compile and still never match —
  axum 0.7 spells a path parameter `:name` — and only driving the router catches that.
- **Deferred:** S3Server session keep-alive (the only remaining control that would need a clock
  inside `VirtualEcu`); per-session timing, which ISO 14229-2 clause 7.5 permits and which the
  0x50 overlay is already the right seam for; AccessTimingParameter (0x83), the in-band way for
  a tester to read and change these; and streaming the frames to the UI over SSE as they are
  emitted, rather than returning them together when the sequence completes.
