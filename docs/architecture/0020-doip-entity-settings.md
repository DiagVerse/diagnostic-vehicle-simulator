# ADR 0020 — Making the DoIP entity's answers editable

Status: accepted

## Context

Response overrides already worked for a DoIP-addressed ECU — the handle work in ADR 0018 saw to
that, and `doip-1030` takes an override exactly as `7E0` does. What could not be changed was
anything the **entity** says, as opposed to what an ECU says:

- the VIN, EID and GID in a vehicle announcement and an identification response could only come
  from a simulation file;
- diagnostic power mode, node type, socket count and maximum data size were hard-coded constants;
- there was no way to make the entity refuse anything.

That last gap is the important one. A tester's handling of a vehicle that will not be discovered,
refuses routing activation, or negatively acknowledges every message is the hardest behaviour to
exercise against real hardware, because you cannot ask a real vehicle to fail on demand. It is
exactly what a simulator is for.

## Decision

**Settings live outside the entity and are shared with it.** `Arc<Mutex<DoIpSettings>>` in
`AppState`, handed to the entity when it starts. So a change takes effect on a running entity and
survives a stop/start, and can be made before anything is listening.

Injecting a fault only by restarting the server would make it useless for reproducing one
*mid-session*, which is when a fault actually matters. There is a test for exactly that: the same
socket, the same tester, activation succeeding and then being refused.

**Everything defaults to the previous behaviour.** An untouched simulation answers exactly as it
did, and `IsInjectingFaults()` reports whether anything is being injected so the UI can say so —
an entity quietly refusing everything because a knob was left on is a confusing afternoon.

**The maximum data size is one number, reported and enforced.** A tester that reads the entity
status and then sends a message that size expects it to be accepted, and a conformance test
cross-checks the two. Deriving the header limits from the setting makes them unable to disagree.

**Undefined codes are refused, not sent.** Only response codes the standard defines are accepted:
routing activation `0x00`–`0x04`, `0x06`, `0x10`; diagnostic NACK `0x02`–`0x06`; header NACK
`0x00`–`0x04`; power mode `0x00`–`0x02`. Fault injection is for reproducing what a real entity
does wrong, not for inventing bytes no vehicle would send — a tester tested against those has
been tested against nothing.

**An injected refusal behaves like a real one.** A forced routing activation denial is still
*applied* to the connection, so the socket closes or is held exactly as the genuine decision
would have left it. A forced header NACK still closes the socket for `0x00` and `0x04` and keeps
it for `0x01`–`0x03`. An injected fault that behaved differently from the real thing would be
worse than no injection.

**A short VIN is refused rather than padded.** Announcing a VIN that is not this vehicle's is
worse than announcing none, and ISO 13400-2 Table 1 already defines how to say "not programmed".

## What this does not cover

Timing faults — a slow identification response, a routing activation that never answers, an
alive check that times out. Those need the entity to be able to delay rather than decide, which
is a different mechanism from a substituted code, and is closer to the ECU timing knobs that
already exist. Worth doing; not folded in here.

## Consequences

- `GET/PUT /doip/settings` and `GET/PUT /simulation/identity`, plus a DoIP tab that covers
  starting the entity, its identity, its parameters and the injection.
- `DoIpEntity::New` takes the shared settings, so every construction site names them.
- **A second Vite proxy gap was found and fixed in the process**: `/doip` was missing from the
  dev proxy entirely, so every one of those calls fell through to the SPA and returned
  `index.html` for the browser to fail to parse as JSON. The same class of bug as `/events`
  before it — a path missing from that list is not an error, which is what makes it easy to
  miss. The comment there now says so.
