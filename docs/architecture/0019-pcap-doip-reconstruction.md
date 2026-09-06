# ADR 0019 — Reconstructing a vehicle from a DoIP capture

Status: accepted

## Context

Plan Phase 7, and the way to validate the DoIP work (ADRs 0015–0017) against traffic that did
not come from us. Everything the simulator could speak DoIP to so far was something it had been
told about; a capture is evidence from a real exchange.

## Decision

**A fourth source, joining at the same place.** `POST /simulation/pcap` →
`SimulationService::LoadFromCapture` → `LoadVehicle`, exactly as the CAN log and simulation file
do. Nothing downstream knows where a vehicle came from.

**The correlation machinery is not shared with the CAN pipeline, and the behaviour extraction
is.** That split is the whole design.

`pipeline.rs` has to *infer* which identifier answers which, because a CAN bus does not say —
hence `DeriveResponseCanId`, the normal-fixed address swap, the most-recent-request fallback. A
DoIP diagnostic message carries its source and target addresses explicitly. There is nothing to
derive and no heuristic to apply: a request is `SA = tester, TA = ecu` and its response is the
reverse pair. Borrowing the CAN correlation would mean carrying a pile of machinery that answers
a question already answered.

What *is* shared is `behaviour.rs` — `ApplyPair` and the `Apply*` family, moved out of
`pipeline.rs` unchanged. Those take an ECU and two byte slices and know nothing about transport.
Sharing them is what guarantees a data identifier learned over DoIP is recorded exactly as one
learned over CAN, rather than by two implementations that agree today.

**An ECU appears only if it answered.** The same rule the CAN path uses. The reference capture
addresses `0x9999` and gets a NACK; that is not an ECU, and inventing one would put a device in
the model that does not exist.

**Acknowledgements are not responses.** `0x8002` says a message was routed, not what an ECU did.
Correlating on it would attribute an answer to the gateway instead of the server.

**A TCP sequence gap abandons the stream.** Same discipline as the ISO-TP consecutive-frame check
(ADR 0011): a PDU assembled across a hole is the right length and the wrong content, and
reconstruction would record it as observed ECU behaviour. Exact retransmissions are skipped
rather than duplicated.

**One network, and it is honest.** The CAN path creates none, because a capture at one connector
cannot observe bus membership. An Ethernet capture *can* observe something real: every one of
these logical addresses was reached at one IP endpoint. So one `EthernetDoIp` network is
recorded, named for that address, marked as the entry point, at `Confidence::Observed`. Whether
any ECU sits behind a gateway remains unknowable, and the topology says so in its own caveats.

**An unprogrammed VIN is not recorded as a VIN.** ISO 13400-2 Table 1 fills an unset VIN with all
zeroes or all `0xFF`; storing that would turn an absent value into a wrong one.

**Refusals are named.** A capture with no DoIP traffic reports how many packets it held and how
many were encrypted, rather than returning an empty vehicle someone has to diagnose. TLS on 3496
is counted and reported, never guessed at.

**Base64 on the wire, hand-decoded.** A capture is binary and every other body on this API is
JSON text. The decoder is a dozen lines and a browser sending a file is the only caller.

**The CLI sniffs the file.** `dvsim reconstruct` reads bytes rather than text and picks the path
from the magic — asking the user to say is asking them to know something the bytes already state.

## Verified

Against the reference capture end to end:

```
1. import over HTTP    handle=doip-1234  doip=1234  ECU_1234  DIDs=[0xF190]
2. the model answers   22 F1 90 -> 62 F1 90 … "1HGBH41JXMN109186"
3. on a real wire      routing activation -> 0x10, ack from 0x1234, same VIN
4. UDP identification  VIN 1HGBH41JXMN109186, LA 0x1234,
                       EID 00 11 22 33 44 55, GID AA BB CC DD EE FF
```

A capture of one vehicle becomes a vehicle that answers a real tester with what the capture
recorded. That is the loop the whole feature exists for.

## Consequences

- `reconstruct` gained `doip` and `pcap` as dependencies, and `ReconstructError` gained
  `Capture` and `NoDoIpTraffic`.
- `behaviour.rs` is now the single definition of what an exchange means; a change there affects
  both sources, which is the point.
- Gateway topology is still not recoverable from a capture, and neither is anything about CAN.
  Merging a capture into an existing vehicle is Phase 10.
