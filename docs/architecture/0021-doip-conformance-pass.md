# ADR 0021 — What the DoIP entity owes a tester, and what it does not

Status: accepted

## Context

ADRs 0015–0020 built the DoIP stack: the codec, the connection state machine, the entity, the
capture reader, and the settings that let an operator make it misbehave on purpose. A
conformance audit against ISO 13400-2:2019 then went through it requirement by requirement and
found a set of places where the entity told a tester something the standard does not say.

None of them were crashes. Every one was a plausible-looking answer that a tester would believe:
a target that does not exist, a socket that is alive but answers nothing, a message discarded
without a count. Those are the expensive kind, because the person debugging trusts the
simulator's account of what happened.

This ADR records the decisions the fixes rest on, not the fixes themselves — those are in the
commit and in the tests, which say what they pin and why.

## Decision

**A functional group address reaches the whole vehicle, and group membership is not modelled.**
ISO 13400-2 Table 13 reserves `0xE000`–`0xEFFF` for functional group logical addresses, and
Table 22's worked example is a tester reading an InfoType from the whole vehicle at `0xE000`.
The entity previously answered these with NACK `0x03`, unknown target address, which tells a
tester no such target exists when in fact every ECU is one.

They are now routed as a broadcast to every ECU in the vehicle — directly to the ECUs the entity
addresses, and through a gateway that is switched on to the ECUs behind it, which is the routing
REQ 7.DoIP-072 AL's own example describes. What is deliberately *not* modelled is which ECUs
belong to which group: nothing in the vehicle model says, and no source the model is built from
carries it. Every group address therefore reaches every ECU. Inventing a membership would be a
claim, and this project's rule is that a reconstructed fact is either observed or absent.

**The transport's limits belong to the transport, not to the addressing mode.** REQ 7.DoIP-072
AL refuses a functionally addressed message that exceeds what a sub-network it must cross can
carry, and its example is CAN: ISO 15765-2 allows a SingleFrame only for a functional request,
because there is no single peer to send flow control. So a functional request over nine bytes is
refused with NACK `0x04` **when some ECU in the vehicle has no DoIP logical address of its own**
— that ECU is behind a gateway on CAN, and the message has to cross it. A vehicle the entity
reaches entirely over Ethernet has no SingleFrame to fit into and is not length-limited.

The whole message is discarded rather than delivered to the ECUs that could have taken it. Half
a broadcast is worse than none: the tester has no way to learn which half.

**An answer with no address to travel under is dropped, not relabelled.** Each ECU's answer to a
broadcast names its own logical address as the source, because several answer at once and the
source address is the only field that separates them. An ECU reached over CAN has no DoIP
address, so there is nothing truthful to put there — its answer is dropped, exactly as a real
gateway drops a reply it has no route back for. The request still reached it and its state still
changed; what is lost is the answer. Falling back to the group address, which is what the code
did first, would have told the tester `0xE000` answered — which no ECU did.

**The two inactivity timers are not the same timer.** The general timer resets "whenever data is
received or sent over this socket" (REQ 3.DoIP-080 NL) — *data*, not *valid messages*, so a
stream of rubbish keeps an activated socket alive while the entity negatively acknowledges every
message of it. The initial timer has no such rule: clause 12.6.3 calls it "a measure against
connection attempts on TCP_DATA sockets with invalid DoIP messages or without sending any data",
and REQ 3.DoIP-085 NL stops it only on receipt of a valid routing activation request. Traffic
must therefore *not* reset it, or a tester could hold a socket open forever without ever
activating routing — which is the one thing it exists to prevent.

This pairs with REQ 3.DoIP-131 NL, which is what makes the silence before routing activation
legible: a diagnostic message on an un-activated socket is not answered and not negatively
acknowledged, and what the tester learns from is the socket closing underneath it two seconds
later. That only works if the timer actually reaches the socket, which is why each served
connection now holds a close signal the timer can fire.

**Advertised capacity is enforced capacity.** Routing activation code `0x01` is for "all
concurrently supported TCP_DATA sockets are registered and active", and what fills that capacity
is *registered* sockets, not open ones — the standard requires an `<n+1>`th resource precisely so
a socket can always be accepted and then refused. The number is the entity's own `m_byMaxSockets`,
read at the moment of the decision, so an operator who lowers it to provoke `0x01` is not quietly
told four.

**An ECU's response timing survives the transport.** A response delay or a forced ResponsePending
is only observable if the messages arrive apart. The entity used to flatten an ECU's response
plan into one burst, which hid exactly the P2/P2* behaviour those knobs exist to provoke. Each
reply now carries its offset and the socket layer waits it out — in the socket layer, not the
entity, because the entity decides while holding the simulation mutex and sleeping under that
lock would stall every other connection for one tester's configured delay.

**A vehicle identification response is delayed on purpose.** REQ 8.DoIP-051 APP, and the
standard says why: every entity on the network answers the same broadcast, and replying in the
same instant is what makes the UDP burst that drops the answers on the way back. The delay is
drawn fresh per request from the system clock's nanoseconds. It is not a cryptographic draw and
does not need to be — what is required is spread, not secrecy. The wait happens in its own task,
because one UDP socket serves every tester and holding it for half a second would make this
entity's de-bursting delay every other answer it owes.

**A payload type the reader cannot decode is a message to step over.** In the capture reader,
framing stopped at the first unknown payload type and abandoned the rest of the TCP stream,
silently and uncounted. A DoIP header's length field says how long a message is whatever its type
means. Manufacturer-specific types (`0xF000`–`0xFFFF`) are legal by design and common in OEM
captures, which is exactly where this cost whole ECUs. Only a broken synchronisation pattern now
abandons a stream, because from there the next boundary genuinely is unknown, and the two
outcomes are counted separately in the capture summary.

**ResponsePending is an interim answer in both pipelines.** Retiring the outstanding request on a
`0x78` loses the response that actually carries the data. On a real ECU most reads of any size go
through one, so this was quietly emptying reconstructed models of DID values while still counting
the exchange.

## Consequences

The Reprolog3 reference capture still reconstructs 645 messages into the same six ECUs an
independent ground-truth parser finds — the framing change adds messages, it does not move any.

Two socket-level harnesses (`scripts/doip-loop-test.py`, `scripts/doip-vehicle-test.py`) drive
the entity as a tester does rather than through the HTTP API, and both now assert the refusals
above rather than only the answers. The vehicle harness runs against a 47-ECU simulation file,
46 of them DoIP-addressable and one reachable only over CAN — which is what makes its
functional-addressing checks worth anything, because a two-ECU bench cannot tell a broadcast that
reaches everything from one that reaches some of it.

Still not built, and still deliberately: the power-up announcement (`A_DoIP_Announce_Num`
messages at `A_DoIP_Announce_Interval`), alive-check arbitration, TLS on 3496, and the
authentication and confirmation states. The alive-check decision is the one worth restating: an
entity may send an alive check to a socket holding a source address and free it if it does not
answer. This one refuses instead, which is stricter than the standard permits and never looser —
a tester is told "in use" where the standard would allow "in use", and never the reverse.
