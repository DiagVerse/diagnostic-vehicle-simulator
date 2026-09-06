# ADR 0015 — One ECU, two ways in

Status: accepted

## Context

Groundwork for a DoIP server (ISO 13400-2). Two things blocked it.

`SimulationService` was keyed entirely by request CAN identifier, and `BuildEcuMap` skipped
every ECU without one: *"loaded but not started"*. Simulation files have been able to declare a
DoIP logical address since ADR 0009, and the topology drew those ECUs — correctly placed, and
labelled as not driveable. They could not answer anything.

And `Vehicle` carried no VIN, EID or GID. All three are mandatory fields of the DoIP vehicle
announcement (ISO 13400-2 Table 5), so there was nothing to announce.

## Decision

**An ECU is stored once, under an explicit key.**

```rust
enum EcuKey { Can(u32), DoIp(u16) }
```

Its CAN identifier if it has one, its logical address otherwise. Every other way of addressing
it is an *index* pointing at that key — `m_mapKeyByLogicalAddress` resolves a DoIP target
address to the same stored object.

This is the whole point. An ECU reachable both ways must be **one** `VirtualEcu`. A tester that
enters the extended session over DoIP and then reads a data identifier over CAN has to find it
still in that session. Two instances keyed separately would diverge while looking identical from
outside, which is the worst kind of bug to hand somebody. There is a test named for exactly this
property.

The rejected alternative was synthesising a fake CAN identifier from the logical address — CAN
identifiers stop at `0x1FFF_FFFF`, so a high bit was free. It would have worked and touched
almost nothing. It is also precisely the kind of clever, invisible encoding that the next reader
has no way to discover, so the explicit enum won despite the larger diff.

**Both transports converge on one function.** `ProcessByCanId` and `ProcessByLogicalAddress`
differ only in how they find the target; both end in `ProcessOnKey`. That is what guarantees
the session gate, the response overrides, the timing plan and the on/off switch cannot drift
apart between transports — there is only one copy of them.

**The indexes are rebuilt together.** An ECU added or removed changes what a broadcast reaches
*and* what a logical address resolves to. Letting one go stale would be a silent routing bug, so
`RebuildFunctionalTargets` rebuilds both.

**Two ECUs claiming one logical address are refused at load.** A tester's
`CP_DoIPLogicalGatewayAddress` must resolve to exactly one entity; picking whichever was listed
first would be an arbitrary choice rendered as a fact.

**`VehicleIdentity` holds what a tester is told**: VIN, EID, GID, plus the "further action
required" and "VIN/GID synchronization status" bytes. All optional, and all with ISO 13400-2
Table 1's defined invalidity fill when unset — so an unprogrammed vehicle announces itself as
unprogrammed rather than as something plausible and wrong. A body in white genuinely has no VIN;
the model should be able to say so.

The two status bytes are kept as raw `u8` rather than enums because their upper ranges are
manufacturer-defined, and an enum would forbid modelling a real vehicle. They are also useful
fault-injection knobs: announcing sync status `0x10` is how a vehicle tells a tester to wait and
ask again, and a tester's handling of that is worth exercising.

**A stale invariant was removed.** `ProcessOnEcu` asserted that every started ECU has a CAN
address, which was true only while DoIP-only ECUs were never started. The `expect` is gone; the
identifiers are absent rather than unreadable, and only the CAN transport reads them back.

## What this does not do

No wire protocol. Nothing listens on port 13400 and no DoIP message is parsed or built. This is
the routing layer a DoIP server will sit on, and the topology now says so rather than claiming
these ECUs cannot be reached at all.

## Consequences

- A DoIP-only ECU is started, holds state, and answers — the `Airbag` in
  `samples/gateway-architecture.simfile.json` is now a working ECU.
- The HTTP API still addresses ECUs by CAN identifier, unchanged. `RunningEcus` filters to those
  that have one, so a DoIP-only ECU is absent from it by construction; the topology reads it
  from the vehicle model, which describes it fully anyway.
- `SimulationError` gained `DuplicateLogicalAddress`.
- Every model written before this loads unchanged, as a vehicle with nothing programmed.
