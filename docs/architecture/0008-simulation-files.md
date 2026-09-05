# ADR 0008 — Simulation files, and the buses they make possible

Status: accepted

## Context

A vehicle could come from two places: reconstructed from a CAN log, or built by hand in the UI.
Neither can say how the ECUs are **wired**. A tester-side capture sees one connector and cannot
tell a local ECU from one behind a gateway (ADR 0006 §4); someone clicking ECUs together has
simply not been asked. So the topology view drew a single link and said plainly that it was a
reachability set rather than a bus.

A third source fixes that, and is useful in its own right: a file describing a whole vehicle,
kept under version control, edited by hand, shared between people.

## Decision

### 1. JSON, with its own field names

The file is JSON with a dedicated set of types, not the vehicle model's serialization.

The model serializes with Hungarian field names — `mStrName`, `mU32RequestCanId` — which is
right for an internal document and wrong for one a person types. A separate layer also means
the model can be refactored without breaking files people have written, and it can accept
conveniences the model has no business knowing about: a trouble code written `P0123-11`, a
status byte written `confirmedDTC | testFailed`, a value written as text.

Markdown was considered and rejected: it reads nicely and parses badly, and this document is
consumed by a program far more often than by a person.

### 2. Everything a file states is `Confirmed`

Nothing in it was observed on a bus, but nothing was guessed either — it came from someone who
knows the vehicle, which is the standing a specification has (README §7). That is a stronger
claim than `Observed` and it is the right one: a capture proves what happened once, a file
states what is true.

### 3. Networks exist only when something states them

`Vehicle` gains `m_vecNetworks` and `Ecu` gains `m_optStrNetworkId`, both defaulted.

The rule that keeps this honest: **`None` means "nobody said", not "the default bus".**
Reconstruction produces no networks at all and the hand-builder produces none either. An ECU on
no declared bus is drawn *unassigned* rather than dropped onto one, because those are different
facts and a diagram that conflates them invents a wire nobody observed.

Consequently the topology view has two modes. With networks, it draws one bus per network with
its kind and bit rate, membership `Confirmed`, and says the buses come from the file. Without,
it draws the single link it always did, membership `Inferred`, with the caveats about gateways
and identifier width that ADR 0006 §4 set out.

Bit rate is `Option<u32>` and never defaulted to a plausible-looking 500 kbit/s. A capture
cannot observe it; guessing would turn "unknown" into a claim.

### 4. A bare string is hex; text has to say so

`"0110"` is a valid pair of bytes *and* a valid piece of text. Guessing which was meant would
silently produce the wrong VIN, so a bare string is hex — diagnostics work in hex — and
characters are written `{ "text": "…" }`. A bare string that is not hex fails with a message
naming the alternative rather than a parse error.

`deny_unknown_fields` is on: a typo like `"sesions"` is refused rather than silently ignored,
which is the difference between a file that does not work and a file that does the wrong thing.

### 5. The file is validated by the same rules as everything else

A response override in a file goes through the same `Validate` the API calls, so a file cannot
express an exchange the UI would reject. Identifier collisions across ECUs, an ECU on a network
the file never defines, an even requestSeed sub-function (which is a sendKey), an all-zero seed
(which is how an ECU says it is *already* unlocked) — each is refused with the offending value.

## Consequences

- Three sources, and the topology diagram is honest about which one it is looking at.
- The sample vehicle is entirely invented, with a VIN containing `I` and `O` — characters
  ISO 3779 excludes — so nothing in it can be mistaken for a real vehicle's data. That follows
  the rule set when a real capture was removed from this repo: fixtures and samples are
  synthetic.
- **Deferred:** gateways and routing between buses as first-class objects. The model can now say
  an ECU is on a bus, but not that a gateway forwards between two — that needs the routing
  layer README §8 describes, and inventing half of it here would be worse than the current
  honest silence.
- **Also deferred:** exporting a simfile from a loaded vehicle, which would close the loop
  between reconstructing from a capture and keeping the result under version control.
