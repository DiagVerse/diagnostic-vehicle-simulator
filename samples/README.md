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
