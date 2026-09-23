# ADR 0022 — Pacing a link that cannot keep up, and who is allowed to unlock

Status: accepted

Extends ADR 0007 (CAN bridge over serial), which built the link this one learned to measure.

## Context

Two days of work against real hardware — a USB-CAN dongle, a 500 kbit/s bus and a tester that
works perfectly against actual ECUs — produced a set of decisions that until now lived only in
commit messages. They are the least obvious decisions in the project and the easiest to
"correct" back into bugs, which is what an ADR is for.

The symptom that started it: a 386-byte SecurityAccess key (`27 02`) arrived at the simulator
with holes in it, and the simulator called the sequence invalid. The tester was not at fault —
it drives real vehicles. Neither, it turned out, was the reassembler: fed the same 386 bytes
over a lossless in-memory bus with BlockSize 0, it reassembled them perfectly. The fault was in
between, and the simulator's mistake was not the loss itself but **advertising a pacing the link
could not honour and then blaming the tester when it was taken at its word.**

The arithmetic that settles it:

| | |
|---|---|
| SLCAN line, 29-bit frame, 8 data bytes | 27 characters |
| at 8N1 | 10 bits per byte |
| 115200 baud ÷ (27 × 10) | **426 frames/s** — the link |
| 500000 bit/s ÷ 135 bits per CAN frame | **3703 frames/s** — the bus |

The link carries about an eighth of what the bus delivers. A tester told `BlockSize = 0`
("send them all") obediently sends 55 frames back to back, the dongle's buffer overruns, and the
middle of the message is gone.

One detail is worth recording because it cost hours: **uniform loss looks like reordering.**
Survivors in the captured logs arrived about 8.5 frames apart, which is the ratio of the two
times above — 2.34 ms per SLCAN line against 0.27 ms per CAN frame is 8.7, and the observed
stride sits just under it. Because that stride is close to 16/2, and the consecutive-frame
counter is four bits wrapping at 16, surviving frames alternate between the two halves of the
counter. The sequence reads `0, 8, 1, 9, 2, A…`, which looks exactly like frames arriving out of
order and is nothing of the kind. Anyone debugging an ISO-TP sequence error on a slow link will
see this and reach for the wrong explanation.

## Decision

### BlockSize defaults to 1, not to the standard's 0

ISO 15765-2's own default is `0`, and `0` is right when nothing is in the way. It is wrong over
any adapter slower than the bus, and it fails in the worst possible manner: not slowly, but with
the middle of a long request missing — a silent wrong answer rather than a visible delay.

`1` costs a FlowControl round trip per frame, which is slow. It is chosen because it is **the
only value that works on every link without being told anything about that link**, and a
simulator that works slowly everywhere is more useful than one that works quickly on the bench
and loses flash data in the field. It is per-ECU and settable, and the moment the link is known
to keep up it should be raised.

### The bridge never advertises a pacing the link cannot honour

`LinkCapacity` computes both frame rates from the two speeds, and `SafeSeparationTime` returns
the STmin a sender must leave for the link to keep up — or `None` when the link is fast enough
that no pacing is needed. When an ECU asks for no pacing on a link that cannot carry the bus
unpaced, the bridge **overrides the ECU** and advertises the slowest pacing the link does
support, with a warning naming both rates.

Overriding the model is not something this project does lightly; the whole architecture exists so
the model decides. The justification is that flow control is not a statement about the ECU — it
is a statement about *this transmission on this link*, and the ECU cannot know what it is
attached to. Advertising `0` here is not honouring the model, it is making a promise on the
model's behalf that will be broken.

The safe value is rounded up to whole milliseconds (all an STmin below `0xF1` can express) and
then given **one extra millisecond of headroom**. Landing exactly on the computed limit leaves
nothing for a busy host, and being one millisecond slow costs far less than losing a frame.

### The line speed is measured, not assumed

A USB-to-UART bridge reports no baud rate — it generates whichever one it is told to — so the
operating system cannot answer this and neither can a constant. `bridge::probe` sends `V` at
each candidate speed, **fastest first**, and checks the reply *for shape rather than for
presence*: a UART reading at the wrong speed delivers mangled bytes, and mangled bytes are not a
version reply. That check is what makes fastest-first safe.

`BaudRateSource` reports how the number in use was arrived at — `Detected`, `Requested`,
`NoAdapterReply`, `NoRealSerialLine` — because a measured rate and a fallback call for entirely
different next steps when the link misbehaves, and an operator who cannot tell them apart will
debug the wrong half of the system.

### A simulator does not verify a key it was never told

A key policy per security level, three values:

- **`CompareWithExpectedKey`** — the default, and what an ECU with a known key does.
- **`AcceptAnyKey`** — what **every level reconstructed from a capture** gets. A capture shows a
  seed and a key going past; it does not show the algorithm. Comparing against a key we never
  learned would refuse the very tester that recorded the capture, which is precisely backwards.
- **`RefuseWith { nrc }`** — never unlock; answer every key with a chosen code. This is the one
  that makes the refusal path testable: a tester's handling of NRC `0x35` is a real behaviour
  that deserves to be provoked on demand rather than only by accident.

The principle underneath: **the simulator's job is to produce the answer the operator configured,
not to pretend it can verify something it has no basis to verify.** Inventing certainty the
source data does not support is the one thing this project's reconstruction rules forbid, and a
seed/key check is no different from a DID value in that respect.

### Entering the default session relocks security, and drops the seed

ISO 14229-1 §10.3. Called from exactly two places — `SetSession` to default, and
`ResetToDefaultSession` — and deliberately inside the ECU rather than in the UDS plugin, so a
second plugin driving the same ECU cannot skip it.

**Dropping the outstanding seed matters as much as clearing the unlocked level**, and is the half
that is easy to miss. A seed left armed across a session change lets a `sendKey` from the previous
cycle unlock the ECU. Worse, a seed *not* dropped is what makes the next `requestSeed` report
"already unlocked" and answer the following `sendKey` with NRC `0x24` — a failure that appears one
full cycle after the mistake, which is the hardest kind to trace back.

## Consequences

**A long request now survives a slow link by default**, at the cost of a FlowControl round trip
per frame. Anyone benchmarking transfer throughput should raise BlockSize first, and will find
the difference large.

**The bridge can contradict the model**, in exactly one direction and only about pacing. The
warning it logs names both frame rates, so the override is never silent. If a third such override
is ever wanted, that is the moment to reconsider this shape rather than to add it.

**A probe costs about a second at start-up** when nothing answers — seven candidates at 150 ms
each. An adapter that answers is usually found on the first try.

**Reconstructed ECUs unlock for any key.** This is intended, and it means a reconstructed vehicle
is not a security test target. A vehicle that should refuse must be told to, with `RefuseWith`.

**Not addressed here**, and still open:

- The SLCAN `U` command, which would let the engine *command* a faster UART instead of only
  discovering one. The reference dongle leaves `V` unanswered, so its rate falls back to an
  assumed 115200 and the pacing above is then computed from a guess rather than a measurement.
- ~~An "apply this flow control to every ECU" action.~~ Built — `PUT /simulation/flow-control`,
  and a button beside the per-ECU fields. It writes **only** BlockSize and STmin: the response
  delay, the forced ResponsePending and P2/P2* are usually set on one ECU deliberately, and a
  bulk action that undid that while fixing a link would be worse than no bulk action.
