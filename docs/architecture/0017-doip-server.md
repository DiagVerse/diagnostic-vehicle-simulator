# ADR 0017 — The DoIP entity on a wire

Status: accepted

## Context

ADR 0015 made ECUs addressable by DoIP logical address; ADR 0016 built the message codec. This
is the part that listens.

## Decision

**Split the same way the CAN side is.** `entity` decides — it holds the connection table and
answers messages, owning no sockets and no clock. `server` owns the UDP and TCP sockets and does
what it is told. A whole diagnostic session can therefore be driven in a test without a network,
and the tests that *do* use sockets exist to prove the two halves are wired together, not to
re-test the protocol.

**The acknowledgement goes out before the ECU has answered.** ISO 13400-2 REQ 7.DoIP-067 AL: the
positive acknowledgement means *routed*, not *accepted* — it is sent once the message has passed
the routing mechanism and been put into the destination's transmission buffer, before the ECU
has processed anything. A negative response arriving later does not contradict it, and an ECU
that goes quiet after it is a UDS P2 timeout rather than a DoIP fault. Sending the
acknowledgement after the answer would make a conformance test measure P2 against the 50 ms
acknowledgement target and fail it.

**Every step of a response plan is its own diagnostic message.** Including each ResponsePending.
Concatenating them into one payload would hand the tester something it cannot parse as UDS.

**`RoutingOutcome` maps onto DoIP by what actually happened**, and the distinctions matter:

| Outcome | DoIP answer | Why |
|---|---|---|
| `NoTarget` | NACK `0x03` unknown target address | No ECU carries that logical address |
| `Stopped` | NACK `0x06` target unreachable | The entity is not on the air at all |
| `Silenced` | **ACK, then nothing** | The ECU exists and the request *was* routed to it. That is what a real gateway produces, and it leaves the tester to time out on P2 as it should. NACKing here would claim a routing failure that did not happen |
| `Handled` | ACK, then one message per plan step | |

**Nothing is routed before routing is active, and nothing is said about it either.** A diagnostic
message on an unactivated socket is not negatively acknowledged (REQ 3.DoIP-131 NL) — the socket
dies to the initial inactivity timer instead. Answering it would be more helpful and would be
wrong.

**TCP framing comes from the header's length field, never from segment boundaries.** Messages
split across segments and coalesced within one both happen with real testers. The read loop
buffers and re-frames; there is a test that pipelines two messages into a single write.

**Acceptable tester addresses are `0x0E00`–`0x0FFF`.** The standard never enumerates an
accept-list — definition 3.13 says only "not listed in the connection table entry" — so this is
policy. Accepting the reserved client block and refusing the rest means an ECU address used by
mistake as a tester address is caught rather than silently honoured.

**Only activation types `0x00` and `0x01` are accepted.** The manufacturer range is refused with
`0x06` rather than accepted, because answering `0x10` to a type whose meaning we have not
implemented tells a tester that something is configured when nothing is.

**The default entity address is the vehicle's lowest logical address.** Not arbitrary: ISO 13400-2
Table 13 puts the VM-defined gateway block first, so the lowest address is normally the gateway
— the entity a tester expects to reach.

## What is not built

- **No power-up announcement.** The three broadcasts at `A_DoIP_Announce_Wait` + 500 ms intervals
  need a decision about *when* a simulator counts as powered up, and broadcasting from a test
  machine onto a real network is a side effect nobody asked for. Identification requests are
  answered; the unsolicited announcement is not sent.
- **No alive-check arbitration.** Response codes `0x01` and `0x03` are only conformant after
  alive-checking the sockets that would be displaced (REQ 3.DoIP-091 to 3.DoIP-096 NL). Until
  that exists the codes are returned on the simpler condition, which is stricter than the
  standard requires rather than looser — a tester is refused where it might have been admitted,
  never admitted where it should have been refused.
- **No TLS.** Port 3496, the Table 30/31 cipher suites, and routing activation code `0x07` as
  the "come back on the secure port" signal.
- **No authentication or confirmation states.** `0x04` and `0x11` are modelled in the state
  machine but never returned, because nothing decides them yet.

## Consequences

- `POST /doip/start`, `POST /doip/stop`, `GET /doip/status`. Binding to port 0 lets the OS pick,
  which is what the tests use so they cannot collide with each other or with anything already on
  13400.
- A DoIP-only ECU is now reachable by a real tester over a real socket — the `Airbag` in
  `samples/gateway-architecture.simfile.json` answers `22 F1 8C`.
- `VehicleIdentity` reaches the wire, and the simfile can set it. An unset VIN is announced with
  ISO 13400-2 Table 1's invalidity fill rather than as a plausible-looking wrong VIN.
