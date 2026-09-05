# Samples

## Simulation files

A **simfile** describes a whole vehicle in one hand-editable JSON document: its buses, its ECUs,
what each one answers, and how it is wired. Load one from the Simulate tab, or pass it to
`dvsim`.

| File | What it contains |
|---|---|
| `chassis-control.simfile.json` | Two chassis ECUs, `0x700/0x70C` and `0x701/0x70D`, modelled on the shape of an older simulator's `.ecu` file: per-session service availability, override-only services, a deliberately silent identifier, and an ECU whose 900 ms response delay makes it emit NRC 0x78 before answering. |
| `demo-vehicle.simfile.json` | Two buses — a 500 kbit/s powertrain CAN and a 125 kbit/s body CAN — carrying an Engine, a Transmission, a BCM on an OEM identifier pair, and a 29-bit Gateway. Between them they cover DIDs written as text and as hex, trouble codes in the familiar `P0123-11` form, status bits written by name, security access, and a response override for a service the engine's UDS plugin does not implement. |

Everything in it is **invented**. The VIN is deliberately not a valid one — ISO 3779 excludes
`I` and `O`, and this contains both — so nothing here can be mistaken for a real vehicle's data.

### Why a simfile can say things the other two sources cannot

A vehicle can come from three places, and they know different amounts:

| Source | Knows the ECUs | Knows the buses |
|---|---|---|
| A CAN log | Only the ones that answered | **No** — a capture sees one connector and cannot tell a local ECU from one behind a gateway |
| Built by hand | The ones you added | Not asked |
| A simfile | All of them | **Yes** — the author states it |

So the topology diagram draws real buses only for a simfile. For the other two it draws one
reachability set and says so, rather than inventing a wire nobody observed.

### The fields

Everything except `simfileVersion`, `vehicle`, `ecus`, and each ECU's `name`,
`requestCanId` and `responseCanId` is optional.

- **`dids`** — a bare string is **hex**; for characters write `{ "text": "…" }`. `"0110"` is a
  valid pair of bytes *and* a valid piece of text, so guessing would silently produce the wrong
  answer.
- **`dtcs`** — `"P0123-11"`, or a raw `"0x012311"`. `status` takes a preset name, hex, or bit
  names joined with `|`.

  The presets are `neverTested`, `pendingOnly`, `failingThisCycle`, `activeConfirmed`,
  `activeConfirmedWithLamp` and `historyConfirmed` — use one unless you specifically need
  otherwise. **Do not reach for `0x00`**: two of the eight bits mean "has *not* run", so zero
  says "the test ran and never failed", and a tester picks faults by masking — nothing matches
  zero, so a DTC stored that way is invisible to every read. `neverTested` is what "nothing has
  happened yet" actually looks like.
- **`responses`** — an answer to a particular request. A byte written `**` matches anything.
  Leave `response` out to make the ECU stay silent for that request. This is the only way to get
  a positive answer out of a service the engine's UDS plugin does not implement.
- **`sessionServices`** — which services each session allows, keyed by session name. Leave it
  out and every supported service works in every session. A session you *do* list is restricted
  to what it lists, and anything else is refused with NRC `0x7F`
  serviceNotSupportedInActiveSession — that is how a real ECU keeps unlocking, flashing and
  actuation out of the session a tester lands in. A session you do not mention stays
  unrestricted, so locking down `extended` does not silently lock `default` too.

  A `responses` entry always wins over this. An override is you saying, of one exact request,
  "this ECU answers it" — which is more specific than a list describing what the protocol
  offers, and without that rule an override-only service such as `0x2E` could never be reached
  once any session was restricted.
- **`addressing`** — usually omit it; it follows from whether the identifiers are 11- or 29-bit.

Everything a simfile states is recorded as `Confirmed`: nothing was observed on a bus, but
nothing was guessed either — it came from someone who knows the vehicle.


## `gateway-architecture.simfile.json` — a vehicle with depth

The other two samples describe flat vehicles: a tester addresses every ECU directly. This one
describes an architecture, which is what a real vehicle has.

```
Tester
  └── Diagnostic Ethernet          (entryPoint: the tester attaches here)
        ├── Central Gateway         CAN + DoIP, gatewayFor: powertrain, body
        ├── Airbag                  DoIP only
        ├── Powertrain CAN                     (1 gateway deep)
        │     ├── Engine
        │     └── Transmission
        └── Body CAN                           (1 gateway deep)
              ├── Body Control Module
              ├── Body Zone Controller
              └── Chassis Domain Controller    gatewayFor: chassis
                    └── Chassis CAN-FD         (2 gateways deep)
                          └── ABS              29-bit normal fixed addressing
```

Three fields do all of that:

- **`entryPoint`** on a network — the link a tester actually attaches to. Leave it off every
  network and each link nothing gateways onto becomes one, which is why the flat samples need no
  entry point at all. Set it on one and the guess stops running, so you can model a tester
  plugged in behind a gateway if that is what you are doing.
- **`gatewayFor`** on an ECU — the networks it forwards diagnostics onto. This is the whole of
  what makes an ECU a gateway. Depth is worked out from it, not written down, so it cannot go
  stale.
- **`can` and `doip`** on an ECU — how a tester addresses it. Give either, or both. A gateway
  usually has both, since that is what makes it a gateway.

```json
{
  "name": "Central Gateway",
  "network": "diag-ethernet",
  "gatewayFor": ["powertrain", "body"],
  "doip": { "logicalAddress": "0x0010" },
  "can": { "request": "0x7E7", "response": "0x7EF" }
}
```

**An ECU with only `doip` is declared, not simulated.** The engine drives CAN on the wire, so
the Airbag in this sample appears in the diagram — correctly placed, on the right link — with a
note saying nothing is answering for it. That is deliberate: dropping it would hide part of the
vehicle, and pretending it answers would be worse.

**Requests are still routed in one flat CAN-identifier namespace.** The diagram tells you the
ABS sits two gateways back; the simulation answers it as quickly as it answers the gateway. The
architecture is described, not yet enforced.

### The version 1 spellings still work

`requestCanId` and `responseCanId` written flat on the ECU are read exactly as before, so every
file written against version 1 loads unchanged. Writing both spellings on one ECU is refused
rather than resolved — preferring one silently would make the other a lie you never find out
about.

### The same thing without a file

Architecture is not a simfile feature. A vehicle reconstructed from a CAN log arrives with no
buses at all, because a capture taken at one connector cannot observe which bus an ECU is on.
The **Topology** tab's *Edit architecture* panel declares buses and places ECUs on them for any
loaded vehicle, whatever it came from, and the from-scratch builder can declare a bus and place
each ECU as it is added.


### Switching ECUs off

Every ECU in the diagram has a switch. Off is not a diagnostic state — the ECU answers
*nothing*, which is what an unpowered or unfitted ECU does. It is not a negative response, and
it is not a reset: the session and security state are still there when you switch it back on.

Switching off a **gateway** takes everything behind it off the air, which is the first thing the
declared architecture actually enforces. In this sample, switching off the Central Gateway
silences the Engine, Transmission, both body ECUs and the ABS two hops back — while the Airbag,
which sits on the link the tester is plugged into, keeps answering. The request comes back as
`silenced`, naming the gateway that is off, rather than as a negative response.

The same switch is on the ECU cards in the Simulate tab and in the Topology tab's list view; all
three make the same call.


## `uds-superset.simfile.json` — every UDS service, answering

The other samples describe plausible vehicles. This one is a **reference**: one ECU that answers
every service ISO 14229-1 defines, so a tester, a script or a CI suite has something complete to
work against.

All 27 services answer:

| | | | |
|---|---|---|---|
| `10` DiagnosticSessionControl | `11` ECUReset | `14` ClearDiagnosticInformation | `19` ReadDTCInformation |
| `22` ReadDataByIdentifier | `23` ReadMemoryByAddress | `24` ReadScalingDataByIdentifier | `27` SecurityAccess |
| `28` CommunicationControl | `29` Authentication | `2A` ReadDataByPeriodicIdentifier | `2C` DynamicallyDefineDataIdentifier |
| `2E` WriteDataByIdentifier | `2F` InputOutputControlByIdentifier | `31` RoutineControl | `34` RequestDownload |
| `35` RequestUpload | `36` TransferData | `37` RequestTransferExit | `38` RequestFileTransfer |
| `3D` WriteMemoryByAddress | `3E` TesterPresent | `83` AccessTimingParameter | `84` SecuredDataTransmission |
| `85` ControlDTCSetting | `86` ResponseOnEvent | `87` LinkControl | |

…and every sub-function they define — including all 29 sub-functions of `0x19`
ReadDTCInformation, of which the engine's UDS plugin implements exactly one.

### What the plugin does, and what the file does

Worth being precise about, because it decides which parts are *stateful*:

- **`10`, `11`, `22`, `27`, `31`, `3E` and `19 02` are the plugin's.** They are deliberately
  **not** overridden, because they hold real state: `10` actually changes session, `27` actually
  hands out the configured seed and checks the key, `22` reads this ECU's real DID map, `19 02`
  reports its real DTC list. Overriding those would replace a working ECU with a tape recording.
- **Everything else is an override in this file.** Those services are not implemented by the
  engine, so the answer is canned — correct in shape, and fixed in content.

So `27 01` is refused in the default session and grants a seed in the extended one, exactly as a
real ECU does. That is not a gap in the sample; a sample that handed out seeds in the default
session would teach a tester the wrong lesson.

### Two things this needed from the format

Both existed in the engine already and were simply not reachable from a simfile:

- **`matchTrailingBytes`** — treat the pattern as a prefix and accept anything longer. Without
  it an override matches one exact length, so `2E` could be simulated for a three-byte value and
  nothing else. `36` TransferData, `23`, `31`, `34` and `84` all need it.
- **`echo`** — copy runs of request bytes into the response. Real positive responses echo part
  of the request: the DID in a `6E`, the block sequence counter in a `76`, the memory selection
  in a `59 17`. A wildcard override without echo answers every request with one hard-coded
  value, which a tester checking its own echo catches immediately.

```json
{
  "request": "2E ** **",
  "response": "6E 00 00",
  "matchTrailingBytes": true,
  "echo": [{ "requestOffset": 1, "length": 2, "responseOffset": 1 }],
  "note": "Accept a write to any DID and echo the identifier back."
}
```

The `00 00` in the response is a placeholder the echo overwrites — a response is literal bytes,
and only the *request* pattern may carry `**`.

### What it does not do

The canned services acknowledge without acting. `14` ClearDiagnosticInformation answers `54` but
does not clear the DTC list; `2E` answers `6E` with the right DID but does not store the value,
so reading it back returns what the model already held; `2A` acknowledges a periodic request but
nothing is then transmitted periodically. Each is an answer of the right shape, which is what
makes it useful for exercising a tester's parsing, timing and error handling — and not a
substitute for the service being implemented.
