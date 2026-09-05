# ADR 0004 — Routing simulated ECUs by CAN address

Status: accepted (MVP-1)

## Context

ADR 0003 turns a CAN log into a Unified Vehicle Model. MVP-1 has to run that model: hold the
reconstructed vehicle in the engine and answer UDS requests the way the real bus would, so the
UI (MVP-2) and a real USB-CAN dongle (MVP-3) can both drive the same simulation.

The Phase 1 engine had exactly one ECU and no notion of addressing — a request went to the one
ECU that existed. A reconstructed vehicle has several ECUs, and which one answers is decided
entirely by the CAN identifier the request arrives on. That identifier was not in the model at
all, and correlation discarded it.

Three addressing shapes appear in real logs:

| Shape | Request | Response | Source |
|---|---|---|---|
| Legislated 11-bit | `0x7E0..0x7E7` | request + 8 | ISO 15765-4 |
| OEM 11-bit | arbitrary | arbitrary (e.g. `0x745` -> `0x765`) | OEM-specific |
| Normal fixed 29-bit | `<prio>DA<target><source>` | target/source swapped | ISO 15765-2 |

plus the **functional** (broadcast) identifiers every ECU listens on — `0x7DF` for the
legislated 11-bit range, `0x18DB33F1` for 29-bit normal fixed.

## Decision

### 1. The model carries the identifier pair

`core-domain::model::Ecu` gains an optional `CanAddress`: request id, response id, the
functional id the ECU also listens on, the addressing mode, and a `Confidence`. Identifiers are
`u32` (29-bit does not fit a `u16`), and the field is optional because an ECU from a source
that carries no CAN addressing (ODX, DoIP) legitimately has none — such an ECU is loaded but
cannot be reached on CAN.

`Confidence` is part of the address because a derived identifier was never on the bus:

- both identifiers seen in a physically addressed exchange -> `Observed`;
- an ECU seen only answering a broadcast, whose own request identifier was derived from its
  response identifier -> `Inferred`;
- an inference never downgrades an already-observed pair.

### 2. Derivation is range-gated, never a blanket rule

`response = request + 8` is normative in ISO 15765-4 **for `0x7E0..0x7E7` only**. Outside that
range identifier pairs are OEM-specific and follow no rule, so nothing is derived there —
correlation falls back to temporal order instead of pairing wrongly. 29-bit normal fixed is
fully specified, so its target/source swap is safe to derive; it is matched on the `N_TAtype`
byte rather than a hard-coded `0x18DA` prefix, because the priority bits are not fixed.

An ECU seen only on a broadcast whose identifier cannot be derived is left **without** a CAN
address rather than having the shared broadcast identifier recorded as its own — that would
collide with every other listener.

### 3. A `simulation` crate owns the running vehicle

`SimulationService` holds the loaded `Vehicle` and one stateful `VirtualEcu` per request
identifier. It lives in its own crate because it needs the `ecu` runtime and `ecu` already
depends on `application` for the `ProtocolHandler` port; putting it in `application` would form
a dependency cycle.

Routing has exactly three branches:

1. **Physical** — one ECU owns the identifier; it answers on its own response identifier.
2. **Functional** — every ECU listening on the identifier processes the request on its own
   state, answers on its own response identifier, and the answers come back in ascending
   response-identifier order (CAN arbitration is won by the lower identifier).
3. **Neither** — nothing is transmitted. An identifier in no ECU's acceptance filter is not
   received by any ECU, so there is no server to produce a negative response. Silence, not an
   NRC.

### 4. Broadcast NRC suppression lives in the router, not the protocol plugin

ISO 14229-1 (clause 7.5.3.3 Table 5, clause 7.5.4.3 Table 7) requires a functionally addressed
server to suppress NRCs `0x11`, `0x12`, `0x31`, `0x7E` and `0x7F`, so a broadcast does not draw
a chorus of "I do not support that". This is filtered after the plugin answers rather than
inside it: the UDS plugin is deliberately transport-agnostic, and `REcuSnapshot` is a
stable-ABI struct, so adding an "is functional" field would break every built plugin. An
addressing-layer rule belongs in the addressing layer.

The state change always applies, suppressed or not — suppression hides the response, never the
transition.

### 5. Extended and mixed addressing are out of scope, and must fail loudly

Under extended and mixed addressing the ISO-TP PCI sits at `data[1]`, not `data[0]`. The
`isotp` crate reads it unconditionally from `data[0]`, so such a frame would not error — it
would produce plausible garbage. `CanAddressingMode` therefore has two variants only, and any
future third one must be rejected at load rather than routed.

## Consequences

- A single service serves both the virtual path (HTTP) and, later, the hardware bridge; the
  simulation stays state-aware rather than replaying a log (README §13).
- Request SIDs are a whitelist (`0x10..=0x3E`, `0x83..=0x88`, ISO 14229-1 clause 7.3) rather
  than "anything below 0x40", so ordinary periodic traffic — most of a real capture — is not
  mistaken for diagnostics.
- `SimulationService` answers with **PDUs**, not CAN frames. ISO-TP segmentation (the transmit
  direction) does not exist yet; MVP-3 adds it for the hardware bridge.
- Deferred, and recorded here so they are not lost: reassembled PDUs are ordered by their first
  frame rather than their last (a 233 ms multi-frame request can therefore sort after responses
  that really followed it); ISO-TP consecutive-frame sequence numbers are not validated; the
  Vector `.asc` `x` extended-frame marker is parsed away; and an 11-bit ECU's
  `m_u16LogicalAddress` still stands in as its response identifier, which is a CAN id, not a
  DoIP address.
