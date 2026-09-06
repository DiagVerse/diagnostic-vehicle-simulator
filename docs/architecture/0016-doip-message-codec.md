# ADR 0016 — The DoIP codec, and why it has no sockets

Status: accepted

## Context

ISO 13400-2:2019 has a lot of surface: four generic-header rejection paths with different
consequences, sixteen payload types with per-type length rules, a routing activation decision
table spanning nine cases, seven diagnostic negative acknowledgements of which exactly one
closes the socket, and eight timing parameters.

Most of what a DoIP implementation gets wrong is not the networking. It is the details — and
they are the details a conformance test is built to find.

## Decision

**The codec and the state machine own no sockets and no clock.** `crates/doip` turns bytes into
messages and back, and decides what a connection should do next; it never reads or writes one.
Time enters through `Connection::Tick(elapsed_ms)`, which the caller drives.

This is the same split as `isotp` and `slcan` against `bridge`, and it is worth restating why:
the routing activation decision table is where this standard is most intricate, and every case
in it is reachable from a unit test in microseconds. Behind a socket, several of them need two
concurrent testers and a stopwatch.

**Rejection reasons are the wire codes, and they carry their own consequence.** `HeaderNack` and
`DiagnosticNack` are enums whose variants map one-to-one onto the byte that goes back, and each
answers `ClosesSocket()` itself.

That pairing is the point. Header NACKs `0x00` and `0x04` close the socket; `0x01`, `0x02` and
`0x03` discard the message and keep it. Among the diagnostic NACKs, only `0x02` closes — and
that rule appears **only in the requirement text (REQ 7.DoIP-070 AL)**, never in Table 26, which
has no required-action column at all. Deciding it next to the code rather than at whichever call
site happens to be handling the error is what stops it being got backwards.

**The acknowledgement builder does the address swap itself.** Table 23 makes a diagnostic
acknowledgement's source the intended *receiver* of the message being acknowledged, and its
target that message's *sender*. Echoing the original pair unchanged is the single most common
bug in this payload. `BuildDiagnosticAck` therefore takes the original message rather than two
addresses, so a caller cannot get the order wrong.

**`0x02` and `0x03` stay distinct.** `0x02` is "larger than I can ever accept" — a fixed
capability, and decidable from the eight header bytes alone, which is what stops a header
claiming four gigabytes from being allocated for. `0x03` is "larger than I can accept right
now". Collapsing them is common and conformance tests separate them.

**A vehicle identification request's protocol version is ignored outright.** REQ 7.DoIP-156 AL:
a tester that has not yet discovered the vehicle cannot know what version to use, which is what
the `0xFF` placeholder is for. The synchronisation check still applies; the version value does
not. But `0xFF` is never echoed — the answer uses this entity's real version.

**Only the two mandatory routing activation types are accepted.** `0x00` default and `0x01`
required-by-regulation. The manufacturer-specific range above `0xE0` is refused with `0x06`
rather than accepted, because answering `0x10` to an activation type whose meaning we have not
implemented tells a tester something is configured when nothing is.

**Both conformant lengths of every optional tail are accepted.** An announcement is 32 bytes or
33; a routing activation request 7 or 11; an entity status response 3 or 7. Accepting one of
each pair is a real interoperability failure, because testers send both.

## Deliberate deviations, stated

- **A diagnostic message must carry at least one byte of user data.** Table 21 sets no minimum,
  so a four-byte payload is arguably legal — but a diagnostic message with no service identifier
  cannot be routed anywhere, and refusing it gives the tester a precise answer instead of
  silence.
- **Reply protocol version** is not specified by the standard. Answering in the version received
  is what keeps a 2012-era tester working, so that is what happens, with `0xFF` excepted.
- **Which source addresses are acceptable** is left to the caller. The standard never enumerates
  them (definition 3.13 says only "not listed in the connection table entry"), so it is policy,
  not protocol.

## What this does not do

No sockets: nothing listens on 13400, no announcement is broadcast, and the alive-check
arbitration that decides between response codes `0x01` and `0x03` needs several concurrent
connections and therefore belongs to the server. TLS on 3496 is out of scope for now; the port
is named so the number is not invented twice.

## Consequences

- 31 tests, each named for the conformance trap it pins rather than the function it calls,
  because "this function is right" is not the interesting claim.
- The server layer that follows has no protocol decisions left to make — it moves bytes, keeps
  a connection table, and drives the clock.
