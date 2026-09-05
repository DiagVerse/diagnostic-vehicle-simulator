# ADR 0010 — Switching ECUs off, and drawing the vehicle

Status: accepted

## Context

Two requests, and they turn out to be one design.

The first was a picture: a topology that looks like the zonal-architecture diagrams engineers
actually work from — buses as lines, ECUs as coloured boxes hanging off them, gateways with
their sub-networks drawn beneath. The nested-card view added in ADR 0009 carried the right
information but read as a list, not a vehicle.

The second was a switch: turn an ECU off and on, from the diagram or from anywhere else.

They meet because a switch is only meaningful in a diagram that shows what depends on what. On
a real vehicle an unpowered gateway takes every ECU behind it off the air; if the simulator
drew the gateway and then ignored it when routing, flicking the switch would visibly do nothing
to the ECUs beneath, and the picture would be decorative.

## Decision

**Off means absent, not refusing.** A switched-off ECU answers nothing at all — no negative
response, no NRC. That is what an unpowered ECU, or one not fitted to this trim level, actually
does. Answering `0x7F` would teach a tester the opposite of the truth, and testers are exactly
what this simulator exists to exercise.

**Off is not a reset.** Session, security state and configuration are all kept, so switching an
ECU back on resumes rather than restarts. Nothing told it to reset, so it does not.

**A disabled gateway silences everything behind it.** This is the first thing the declared
architecture from ADR 0009 actually *enforces* rather than merely describes.
`DiagnosticPathTo` already walked from an ECU to a tester; it now also reports the disabled
gateway nearest the tester, and routing turns that into silence. The gateway *nearest the
tester* is the one named — with two off, the request never reaches the second, and reporting it
would send someone to fix the wrong ECU.

**A new routing outcome rather than reusing `NoTarget`.** `RoutingOutcome::Silenced` carries
the ECU and the reason. The wire looks identical to `NoTarget` — silence either way — but an
operator who has just flicked a switch needs to be told which of the two they are looking at,
and the UI says so in words.

**A named serde default.** `#[serde(default)]` on a `bool` is `false`, which would load every
model written before this field existed with every ECU switched off. `DefaultIsEnabled` returns
`true`, and a test asserts that a model with the field absent loads switched on — the same trap
`m_u32P4ServerMaxMs` documents.

**The diagram is a deterministic tree layout, not a physics simulation.** A recursive walk
places each bus, centres each gateway over whatever its sub-tree needs, and returns the width
it used, so branches cannot overlap and no collision pass is needed. The same vehicle draws the
same way every time, which matters when someone is comparing two of them.

**Colour is per bus, not per protocol.** The question this diagram answers is "what is on the
same wire". Colouring by kind would give every CAN segment in the vehicle the same colour,
which is the opposite of useful.

**Diagram and list both stay.** The diagram answers "how is this wired"; the list answers "what
exactly is this ECU". Neither substitutes for the other, so the view is a toggle.

## What this does not do

Nothing here claims a physical position. The model records which bus an ECU is on and which
gateway reaches it, and that is exactly what is drawn — the layout is architectural, not a floor
plan. Placing ECUs in vehicle zones would need a `zone` field somebody actually fills in; until
then, inventing positions would be fabricating certainty the source data does not support
(README §7).

Switching a *bus* off is not offered. It would be a reasonable feature, but it is not the same
as switching off its gateway, and there is no evidence yet about which one people want.

## Consequences

- A gateway is now load-bearing: switching one off is a one-click way to reproduce "the whole
  rear of the car went quiet", which is a real and common diagnostic scenario.
- The switch is in three places for one reason — it is the same call: the diagram, the list view
  and the Simulate tab's ECU cards all `PUT /simulation/ecus/:requestCanIdHex/enabled`.
- An ECU addressed only over DoIP has no switch. The engine addresses ECUs by CAN identifier, so
  it has no handle to flick; the box is drawn without one rather than with one that fails.
- `SimulationRequestResultDto` gained `silencedEcuName` and `silencedReason`, both null for
  every other outcome.
