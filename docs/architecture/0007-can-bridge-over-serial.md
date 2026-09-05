# ADR 0007 — Putting the simulation on a wire

Status: accepted (MVP-3)

## Context

Until now the simulated ECUs could only be reached over HTTP. MVP-3 connects them to a serial
port so a real tester can talk to them: a USB-CAN adapter speaking SLCAN on a real bus, or a
virtual port pair so a tester tool on the same machine can drive them with no hardware at all.

That makes the engine a **bus participant** for the first time. Everything before this observed
finished exchanges; now the peer decides how fast it will accept data, and this end must obey.

## Decision

### 1. Four crates, split by what they know

| Crate | Knows about | Testable without |
|---|---|---|
| `slcan` | The ASCII line protocol. Pure codec, no I/O. | anything |
| `serial-can` | Bytes on a port. One file talks to the OS. | hardware (loopback pair) |
| `isotp` | Segmentation and flow control. No clock. | a clock |
| `bridge` | Wiring the three to the simulation. | hardware (mock bus) |

Library crates, not plugins — ADR 0002's reasoning is unchanged, and an async serial bridge
across a stable-ABI boundary would buy nothing.

### 2. The engine authors a plan; ISO-TP decides how it becomes frames

The response plan (ADR 0005) says *when* a PDU is handed to the transport. The ISO-TP
transmitter decides *how* that PDU becomes frames, and takes whatever wall time flow control
demands. The split is not a convenience: on CAN, the client's P2 timer stops at the **first
frame** of the response, so everything the transmitter does afterwards is outside P2 by
construction.

The consequence that matters: **a tester stalling with FS=Wait is not a P2 violation and is
never reported as one.** It consumes the link's own budgets (N_Bs, and how many waits are
accepted) and those are reported in a separate transport-error type — never merged into the
plan's conformance warnings, which ADR 0005 reserves for schedules the engine knowingly
authored wrong.

The plan executor keeps its synchronous sink. The bridge collects each step as it comes due and
segments afterwards, so a dawdling tester cannot delay a later plan step: the ResponsePending
still goes out at 50 ms and the answer is still handed over at 200 ms, exactly as a real ECU
would.

### 3. What the receiver advertises, and why it is nothing

The live receiver advertises **block size 0 and separation time 0**. Those fields exist so a
receiver can declare *its own* buffer and processing limits, and this receiver is a `Vec<u8>` on
a host with gigabytes. Advertising a plausible-looking ECU value would be the engine asserting a
constraint it does not have — the same fabrication README §7 forbids elsewhere. Both are
configurable, for fault injection and for replaying an ECU whose real values a capture showed.

The maximum message length is configurable for the same reason, and it is the only way the
overflow refusal is reachable at all: a classic first frame's length field is 12 bits, so it
cannot announce more than the engine's own ceiling. A real ECU's buffer is smaller, and saying
so is how that is simulated.

### 4. Three rules the tests exist to pin down

- **A stopped simulation sends nothing, not even flow control.** Checked when the frame arrives
  rather than when it is routed. An unpowered ECU does not acknowledge a multi-frame request it
  will never answer, and a half-alive one that does would badly mislead anyone debugging.
- **Flow control goes on the identifier the ECU answers on**, not the one the request arrived
  on: that is where the tester's transmitter is listening.
- **A multi-frame request on a broadcast is dropped silently.** There is no single peer to flow
  control, and several ECUs answering with one at once would collide.

A frame arriving while an ECU is mid-transfer is dropped and logged — a server part-way through
answering has told the tester it is busy, and interleaving two messages on one identifier is
unrecoverable. That is the CAN equivalent of the HTTP path's 409.

### 5. A reserved separation time means the slowest, not the fastest

`0x00`–`0x7F` are milliseconds and `0xF1`–`0xF9` are 100–900 microseconds; everything else is
reserved and must be treated as **127 ms**. This is the rule implementations most often invert,
and inverting it floods a tester that was explicitly asking to be slowed down.

The raw byte is stored rather than a decoded duration: round-tripping `0xF1` through a
millisecond field would lose the 100 microseconds and re-emit `0x00`, silently changing what
the ECU says about itself.

### 6. Virtual ports are a first-class path, not a test fixture

A pseudo-terminal has no line speed, no parity and no flow-control lines, so the ioctls a serial
library applies come back as "not a typewriter" and the open fails outright. Opening the device
as a plain file avoids all of it and loses nothing, because there is no UART to configure.

`OpenPort` tries a real serial port first and falls back, logging which happened. That path is
how a tester tool on the same machine connects, which makes it worth as much as the dongle path.

### 7. Padding defaults to 0xAA

ISO 15765-2 does not mandate a value. `0xAA` is `10101010`, which produces no bit stuffing —
`0x00` and `0xFF` lengthen the frame on the wire — and it distinguishes engine-generated padding
from the `0x55` a tester commonly uses when reading a capture.

## Consequences

- The whole path is tested at frame level over an in-memory bus, so CI needs no hardware and no
  external tooling. It was additionally verified against a tester at the far end of a PTY pair.
- **What a loopback proves:** that encode → decode → ISO-TP → routing → plan → ISO-TP → encode
  is self-consistent and produces the intended bytes. **What it cannot prove:** arbitration, ACK
  behaviour, bit timing, error frames, or that a specific dongle accepts this command order.
  A real adapter is still required to claim any of that.
- **Deferred:** the adapter role, where the engine presents *itself* as a CANable so a tester
  library like python-can can open it directly — the codec is shared, only the role differs. Also
  deferred: an ELM327 shim, which would move the transport boundary and belongs in its own
  module; CAN-FD, whose SLCAN extensions are mutually incompatible across firmwares; and
  adapter status polling, which is the only honest window onto bus health.
- Several ISO 15765-2 details here were written from working knowledge rather than verified
  against the standard, which was not available. They are marked as such in the code. Check them
  before relying on this for conformance testing.
