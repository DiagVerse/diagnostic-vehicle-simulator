# ADR 0006 — Building vehicles by hand, editing responses, and drawing topology

Status: accepted (MVP-2b)

## Context

Three requests, one phase:

1. A vehicle should be creatable **without a CAN log** — ECUs added one at a time.
2. Any ECU, reconstructed or hand-built, should let a user **edit the response to a request**,
   with the common UDS requests offered by default.
3. There should be a **picture** of how the ECUs are connected.

The first is straightforward. The second and third are not, for the same reason: both are
places where a simulator can very easily start asserting things it does not know.

## Decision

### 1. Declaring a service supported does not implement it

The bundled UDS plugin answers seven services (0x10, 0x11, 0x19, 0x22, 0x27, 0x31, 0x3E). Its
dispatch gates on the ECU's supported-service list *and then* matches on the SID, with a
catch-all returning `serviceNotSupported`. So ticking "supports WriteDataByIdentifier" produces
`7F 2E 11` — byte-for-byte what leaving it unticked produces.

That is the whole justification for response overrides: **for the other services an override is
the only way to get a positive response at all.** The UI says so in those words rather than
letting a user discover it.

A consequence worth naming: an ECU with a response delay and an unimplemented service was
emitting `7F 2E 78` … `7F 2E 11` — a ResponsePending followed by "I don't support that". NRC
0x78 means "this is mine and I am working on it", so the pending count now consults what can
actually answer, not what was declared.

### 2. An override changes what the ECU says, not what it does

The protocol still runs, state changes still apply, and only the transmitted bytes are
replaced. ISO 14229-1 clause 8.2 draws the same line for the suppressPosRspMsgIndicationBit:
"the execution of the service must be completely passed" even when nothing is transmitted.

Matching is a byte pattern with a **per-byte mask**, not exact-only and not regex. Exact-only
fails for exactly the services that need overrides most — a SecurityAccess key, a TransferData
block counter and a memory address all vary per request. Regex invites patterns nobody can
review and has no natural notion of specificity. Masks give `22 ** **` and stop there.

Wildcards force **echo spans**: a response must carry the identifier that was asked for, so a
wildcard read of 0xF18C answered with 0xF190 in it is rejected by any tester correlating on it.

**Most specific wins** — more fixed bytes, then longer, then anchored over prefix — and
deliberately not "last one wins", so reordering a list in a UI can never change behaviour
silently.

Four consequences of the ordering, each with a test:

- Overrides match the bytes **the tester actually sent**, not the copy the engine makes with
  the suppress bit cleared for a ResponsePending sequence. A rule for `3E 80` must not fire on
  a `3E 00` this engine manufactured.
- A response the tester suppressed is **not** answered anyway — the tester is not listening.
  Possible, but only when asked for explicitly, and then it is fault injection.
- An override replaces the **final** response only. The ResponsePending messages belong to the
  timing layer (ADR 0005) and an operator cannot author them; NRC 0x78 is refused outright as
  an override response, because it is a promise to answer that only that layer can keep.
- **The one exception to the slogan:** refusing a session change, reset or security exchange
  rolls the state change back. An ECU that says "I refused" while sitting in the session it
  just entered is incoherent, and every later request would behave inexplicably. Consistency
  between an ECU's words and its state matters more than the slogan.

Silence is a distinct action rather than an empty substitution, because the engine already has
three kinds of silence (suppressed response, withheld final response, no route) and a fourth
has to be nameable in the log or an operator debugging it cannot tell them apart.

Overrides live on the `Ecu`, not the `Vehicle`: a broadcast is processed by every listening ECU
on its own state, so a vehicle-level override would fire on all of them.

### 3. Validation rejects values, not behaviours

Same rule as ADR 0005. Refused: a response that does not answer its request, a negative
response of the wrong shape or echoing the wrong service, a pattern that fixes nothing, a
response byte used as a request SID, NRC 0x78. Allowed: an ECU that refuses a read it should
allow, or goes silent — that is the point of a fault injector.

### 4. The topology diagram claims less than a diagram usually implies

**There is no `Network` type in the model.** So there is exactly one link, and it is labelled
**"diagnostic link, as captured"** rather than a bus.

A tester-side capture has one vantage point: the diagnostic connector. Frames from an ECU
behind a gateway arrive there with nothing distinguishing them from local traffic. Appearing in
one capture proves ECUs are **reachable through the same tester connection**, not that they
share a wire — so link membership is `Inferred`, never `Observed`.

Specifically, identifier width proves nothing in either direction. One CAN 2.0B segment carries
11-bit and 29-bit frames routinely, and a gateway can present three segments' ECUs on one
connector all in 11-bit. The diagram therefore neither splits nor merges on addressing mode; it
says so, on screen, when both are present.

Each node renders the **confidence of its identifier pair** — an inferred pair is drawn dashed.
A diagram that draws a derived fact identically to a witnessed one launders an inference into a
claim.

The tester is drawn as a connector, never as an ECU and never in `m_vecEcus`: its addresses are
a source address and broadcast targets, not a node.

Never drawn, because a tester-side log records accepted frames only: bus load, termination,
error state, silent nodes, and gateways (whose only honest in-band evidence is NRC 0x25
noResponseFromSubnetComponent).

### 5. A default hand-built ECU answers plausibly and asserts nothing

Services default to what the plugin implements, sessions to Default and Extended, and a
hand-stated CAN address is `Confirmed` — the user asserted it, so it is neither observed nor
guessed.

The demo ECU's invented VIN and DTC were labelled `Observed`, which is the fabricated certainty
README §7 forbids; they are now `Unknown`, and its VIN is 17 characters as ISO 3779 requires.

## Consequences

- Overrides are applied inside `VirtualEcu`, so the dev `/ecu/*` path and the `/simulation/*`
  path cannot disagree about what an ECU answers.
- Overrides serialize with the vehicle, so an edited model survives save and load.
- **Deferred, and worth stating so nobody assumes otherwise:** an override is stateless byte
  substitution, so the flashing sequence 0x34 → 0x36×n → 0x37 can be faked for one known pass
  and no further — a real flashing tool with its own block size diverges at the first
  TransferData. Proper flashing needs a protocol plugin.
- **Also deferred:** a `Network` type (kind, bit rate, membership with its own confidence)
  is what would let the diagram show more than one link honestly; overrides that declare a
  state effect explicitly, which is the honest way to make a faked SecurityAccess actually
  unlock; and `m_u16LogicalAddress` still standing in as an 11-bit ECU's response identifier,
  which is why the UI never labels it a DoIP address.
