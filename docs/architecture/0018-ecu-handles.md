# ADR 0018 — Naming an ECU that has no CAN identifier

Status: accepted

## Context

ADR 0015 made ECUs addressable by DoIP logical address, and ADR 0017 put a DoIP server on a
wire. But the HTTP surface was still keyed entirely by CAN request identifier:
`/simulation/ecus/:requestCanIdHex/{overrides,timing,name,enabled,placement}`, and
`SimulationService::RunningEcus` filtered to ECUs that had one.

So a DoIP-only ECU was routable and invisible at the same time. It answered a request over the
wire, and could not be listed, renamed, timed, overridden, switched off or placed — because
there was no way to *name* it in a URL.

That was tolerable while such ECUs were rare. It stops being tolerable with pcap import
(ADR 0019), where a vehicle reconstructed from an Ethernet capture is **entirely** DoIP-only
ECUs: it would import successfully and then present an empty ECU list.

## Decision

**One handle that can name either kind of ECU.** `EcuKey` was already exactly that type
internally; this exposes it.

- A bare hex number is a CAN request identifier: `7E0`, `18DA28F1`.
- `doip-1234` is a DoIP logical address.

**A CAN ECU's handle *is* its identifier**, so every URL that worked before this existed still
resolves — the change is additive at the boundary even though it is a rename underneath.

**Parsed in one place.** `ParseEcuHandle` replaces the `ParseCanId` call in each of the eight
handlers. Eight slightly different parsers is how one of them ends up accepting something the
others reject.

**The per-ECU service methods take an `EcuKey`, not a `u32`.** `RemoveEcu`, `RenameEcu`,
`SetEcuOverrides`, `EcuOverridesOf`, `SetEcuTiming`, `EcuTimingOf`, `SetEcuEnabled` and
`SetEcuPlacement` all changed signature, and `IsAddressedOn` became `MatchesKey`. Passing the key
down rather than translating at the edge means there is no second place where "which ECU is
this?" is decided.

`MatchesKey` refuses to match a DoIP handle against a CAN-only ECU's `m_u16LogicalAddress`, which
is a placeholder rather than a routable address. Matching it would let `doip-07E8` reach an ECU
with no DoIP presence at all.

**`RunningEcus` yields `(EcuKey, &VirtualEcu)`** instead of filtering to CAN. The CAN bridge now
skips DoIP-only ECUs explicitly — it is a CAN bus, and an ECU with no identifier is not on it,
which is a statement about the wire rather than a limitation.

**`POST /simulation/request` accepts a handle too.** Without it the UI would list ECUs it could
not query, which is the same invisibility one level up.

**On the wire**, `SimulationEcuDto` and `TopologyNodeDto` gain `handle`; `requestCanIdHex` and
`responseCanIdHex` become nullable, and `logicalAddressHex` is added. The UI addresses ECUs by
handle everywhere and labels a DoIP ECU by its logical address.

## Consequences

- A vehicle of DoIP-only ECUs is fully usable: listed, queried, overridden, timed, switched off,
  placed in the topology. Verified against the gateway sample — the DoIP-only `Airbag` answers
  `22 F1 8C`, takes an override, and goes silent when switched off.
- Existing CAN URLs are untouched. The route parameter is still spelled `:requestCanIdHex`; only
  what it accepts has widened. Renaming it would break nothing but would misdescribe it, so it is
  worth revisiting when something else touches those routes.
- `SimulationError::EcuNotFound` now carries the handle as text rather than a `u32`, so the
  message names what was actually asked for.
