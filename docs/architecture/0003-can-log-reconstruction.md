# ADR 0003 — CAN-log reconstruction pipeline

Status: accepted (Phase 2)

## Context

Phase 2's goal (README §12) is to turn a recorded CAN log into a Unified Vehicle Model: given
frames captured on a diagnostic bus, discover the ECU(s) and the diagnostic behaviour they
exhibited, marking every reconstructed fact with `Confidence::Observed`.

A diagnostic exchange on CAN looks like:

```
0x7E0 -> 10 03            (tester: DiagnosticSessionControl extended)
0x7E8 -> 50 03 ...        (ECU:    positive response)
0x7E0 -> 22 F1 90         (tester: ReadDataByIdentifier VIN)
0x7E8 -> 62 F1 90 ...     (ECU:    positive response, multi-frame via ISO-TP)
```

Requests and responses use different CAN IDs (commonly request `0x7Ex`, response `0x7Ex+8`),
and payloads longer than 7 bytes are split into ISO-TP (ISO 15765-2) segments.

## Decision

A four-stage pipeline, each stage a small testable unit:

1. **Parse** (`canlog` parser): read the log file into a time-ordered list of `CanFrame`
   `{ timestampMs, canId, data, isFd }`. Support the two most common text formats first:
   - Vector **`.asc`** (`<time> <channel> <id> Rx/Tx d <len> <bytes...>`).
   - Linux **candump** (`(time) iface id#hexbytes`).
   - **Timestamped diagnostic trace** (`HH:MM:SS.ffffff>>0x18dad4f1 -> 02 10 01 55 …`), the
     format service tools produce. It is the only supported format that records each frame's
     **direction** (`>>` tester to ECU, `<<` ECU to tester); correlation believes that marker
     outright and falls back to the service-identifier heuristic only for formats without one.
     Wall-clock timestamps carry no date, so they become seconds since midnight.

2. **Reassemble** (`isotp`): group frames by CAN ID and run ISO-TP reassembly per ID, yielding
   complete UDS PDUs `{ canId, timestampMs, bytes }`. Single-frame (SF), first/consecutive
   (FF/CF) are handled; flow-control (FC) frames are recognised and skipped for offline
   reassembly (we are an observer, not a participant).

3. **Correlate** (`reconstruct`): pair request PDUs with the response PDUs that follow them on
   the partner CAN ID. The request/response ID pairing is inferred (default rule:
   response = request + 8, the 11-bit UDS convention; configurable/generalisable later).

4. **Populate** (`reconstruct`): decode each request/response pair as UDS and update the
   Vehicle model:
   - `0x10/0x50` → add the session to `supportedSessions`.
   - `0x22/0x62` → add the DID and its observed value.
   - `0x19/0x59` → add the reported DTCs.
   - `0x27/0x67` → note a security level (seed observed; key/algorithm stays `Unknown`).
   - `0x31/0x71`, `0x3E/0x7E`, `0x11/0x51` → record the service as supported.
   - Any positive/negative response for a SID → mark that SID `supportedServices`.
   Each ECU is keyed by its response CAN ID → logical address (the low bits, e.g. `0x7E8`→`0x1008`
   as a placeholder until ODX/DoIP give the true address). Everything is `Observed`.

## Confidence & specification-vs-observation

Reconstructed facts are `Observed`. When an ODX/PDX populator later contributes the same
facts as `Confirmed`, the merge step (a later phase) reconciles them into `CONSISTENT` or
`CONFLICT` (README §6). The model already carries per-fact confidence so this needs no
reshaping.

## Non-goals for Phase 2

- Live participation on a real/virtual bus (flow control, timing) — offline reconstruction
  only. Running the reconstructed ECU over a live CAN+ISO-TP bus for an external tester is a
  follow-up.
- CAN-FD-specific decoding beyond carrying the `isFd` flag and larger payloads.
- Functional (broadcast) addressing and multi-ECU gateways — single physical request/response
  pairs first.

## Verification

- Golden test: a checked-in sample log reconstructs to an expected model (ECU address,
  services, DIDs, DTCs, sessions).
- Round-trip test: build a `VirtualEcu` from the reconstructed model and replay the log's
  request PDUs through the `uds` logic; the responses must match those observed in the log.
