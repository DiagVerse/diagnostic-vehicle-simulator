# ADR 0009 — Vehicle architecture: gateways, entry points, and CAN/DoIP as a superset

Status: accepted

## Context

Until now a loaded vehicle was a flat list of ECUs plus, optionally, a flat list of buses. That
matched what the code could honestly claim: a CAN capture is taken at one connector, so ECUs
appearing in it are reachable through the same tester connection and nothing more (ADR 0003).
Simulation files (ADR 0008) added declared buses, but still with no notion of one bus sitting
behind another.

Real vehicles are not flat. A tester attaches to a diagnostic socket or an Ethernet interface,
reaches a gateway, and everything else is reached *through* it. Two things follow that the flat
model could not express:

1. **Depth.** "The tester can address this ECU directly" and "every request to it crosses the
   gateway" are different facts, and a diagram that draws them identically is misleading.
2. **Mixed transports.** A gateway is typically addressed over DoIP *and* on CAN, and the ECUs
   behind it may be either. The model required a CAN identifier pair on every ECU, so a
   DoIP-addressed ECU could not be described at all.

A reference file supplied for this work made the second point concrete: despite being named for
DoIP, every ECU in it carried an `ISO_15765_2_2016` CAN address alongside a DoIP logical
address. Its `osiProtocolStack` was byte-identical across all 48 ECUs, so it carried no
per-ECU network distinction — the architecture was implied only by naming conventions and the
logical addresses. That is exactly the information a format should state outright.

## Decision

**Architecture lives in `core-domain`, not in any one source.** `Vehicle` gained
`ValidateTopology`, `NormalizeEntryPoints`, `NetworkDepths` and `DiagnosticPathTo`. A simulation
file, a hand-built vehicle and a log-reconstructed one all go through the same functions, so
they cannot disagree about what a gateway means.

**Two new facts, minimally expressed.** `Network.m_bIsDiagnosticEntryPoint` marks a link a
tester attaches to. `Ecu.m_vecGatewayForNetworkIds` lists the networks an ECU forwards onto.
Depth is *derived* from those two by breadth-first search rather than stored, so it cannot go
stale.

**One gateway per network.** Two ECUs claiming to forward onto one network is refused. Real
vehicles do have redundant paths, but the model has one path per network, and an ambiguous one
would simply be resolved by whichever ECU happened to be listed first — a silent arbitrary
choice rendered as a fact.

**Entry points are guessed only when nobody has said.** If no network is marked, every network
nothing gateways onto becomes an entry point. That keeps every version 1 file working — such a
file has no gateways, so every bus is directly reachable, which is the truth. If the author
marked even one entry point, the guess does not run: deliberately bench-testing one segment
behind a gateway is a real thing to do, and the default must not overrule it.

**Addressing became a superset.** `Ecu` already carried `m_u16LogicalAddress`; it gained
`m_bHasDoIpAddress` to say whether that address is a real DoIP address or the placeholder a
CAN-only ECU carries. An ECU may now have CAN identifiers, a DoIP logical address, or both. A
flag rather than a second address field, so there is only ever one logical address and it cannot
disagree with itself.

**A DoIP-only ECU is declared, not simulated.** The engine's wire-level simulation is CAN
(ADR 0007), so an ECU with no CAN identifiers cannot be driven. It is loaded into the vehicle,
drawn in the topology and labelled with the reason it is not answering, rather than dropped.
Hiding it would make part of the vehicle invisible; pretending it works would be worse.

**Simfile format version 2.** Version 1 files load unchanged — a flat CAN vehicle is a valid
version 2 vehicle that declares no architecture. `c_uMinSupportedVersion` makes the range
explicit. The version 1 spellings `requestCanId` / `responseCanId` / `logicalAddress` are still
accepted alongside the new `can` and `doip` blocks; giving both spellings is refused rather than
resolved, because silently preferring one makes the other a lie that never surfaces.

**Edits are validated against a copy.** `SimulationService::CommitVehicleEdit` normalises and
validates a cloned vehicle and only then swaps it in, so a rejected change to the architecture
leaves the running simulation exactly as it was.

## What this does not do

Routing is still one flat CAN-identifier namespace. A gateway is drawn, and says which ECUs sit
behind it, but it does not re-address, delay, filter or refuse anything that passes through it —
so a request to an ECU three hops deep is answered as fast as one to the gateway itself. The
topology says so in its own caveats rather than in this file, because the person looking at the
diagram is the one who needs to know.

DoIP is not a transport yet. Declaring a DoIP address places an ECU in the architecture; it does
not make it answer.

## Consequences

- A topology diagram can be nested, and depth is a derived fact rather than a drawn guess.
- Any of the three sources can gain an architecture: `POST /simulation/networks` and
  `PUT /simulation/ecus/:id/placement` are what a log-reconstructed vehicle uses to acquire the
  one thing a capture can never observe.
- A vehicle whose wiring is contradictory — a loop, a gateway onto its own bus, a gateway onto a
  bus nothing defines — is refused at the point of entry rather than rendered.
- A network nothing connects to an entry point is *not* an error. It is drawn, marked
  unreachable, and left for the author to fix; modelling a segment before wiring it up is
  legitimate.
