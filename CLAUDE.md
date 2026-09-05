# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

This is a **greenfield, specification-only** repository. As of now it contains just `README.md` (a detailed design/vision document) and `LICENSE`. There is **no source code, build system, package manifest, or test suite yet**, and the repo is not under git.

Consequently:
- Do not assume or invent build/lint/test commands — none exist. When implementation begins, the toolchain (language, package manager, test runner) is an open decision; confirm it before scaffolding.
- The `Project Structure` tree in `README.md` (§20) is a *proposed* layout, not an existing one. Treat it as a target, not ground truth.
- `README.md` is the authoritative source for intended behavior, scope, and architecture. Read it before making design decisions.

## What this project is

A platform to **reconstruct and simulate a virtual vehicle** for automotive diagnostics — so diagnostic software, flashing tools, and test frameworks can talk to it over real protocols without physical ECUs. Inputs come from either specifications (ODX/PDX, ARXML, DBC) or observed traffic (DPDU diagnostic logs, CAN/CAN-FD logs, PCAP/PCAPNG), or a hybrid of both.

## The one design principle that governs everything

> **Do not build separate simulators for ODX, CAN logs, PCAP, and DPDU. Build one Unified Vehicle Model and provide multiple ways to populate it.** (README §23)

Every architectural choice should preserve this. Parsers and log analyzers are *populators* that feed a single common vehicle representation; the simulation engine consumes only that model. Adding a new input format or protocol must not require a parallel simulator — it should extend the model or add a populator/protocol module.

## Architecture (the layers to hold in your head)

Two flows meet at the Unified Vehicle Model:

1. **Input / Reconstruction flow** (populates the model): source artifacts → parser/extractor → (for logs: protocol detection → decoding → request/response correlation → ECU discovery → behavior extraction) → Vehicle Model.
2. **Simulation flow** (executes the model): Vehicle Model → Simulation Runtime, exposed over CAN / DoIP / J1939 to an external diagnostic tester.

The proposed runtime stack, top to bottom (README §17):
- **Vehicle Model Layer** — ECU, Gateway, Network, Routing, Diagnostic Endpoint
- **Diagnostic Behavior Layer** — Sessions, Services, DIDs, DTCs, Security, Routines
- **Protocol Engine** — UDS, DoIP, ISO-TP, CAN, CAN-FD, J1939, TCP/IP
- **Simulation Runtime** — State, Timing, Events, Scheduler, Fault Injection
- **Communication Layer** — Virtual CAN, TCP/IP, Ethernet, real CAN interface

Key cross-cutting design rules from the README:
- **ECUs are stateful, not packet replayers.** A virtual ECU maintains diagnostic state (e.g. Default → Extended session → Security unlocked) and computes responses from `current state + request + timing + scenario + fault config`. Replay must be **state-aware**, not static playback (§13).
- **Routing is modeled independently of transport.** Diagnostic paths (tester → gateway(s) → ECU, possibly multi-hop across DoIP and CAN) are first-class and separate from the protocol layer (§8, §11). Gateways are first-class simulation objects with configurable routing rules.
- **Specification vs. observation is tracked explicitly.** The model records what a spec says *and* what a trace showed, flagging `CONSISTENT` / `MISMATCH` (§6).
- **Reconstruction is confidence-based.** Every reconstructed fact carries a state: `CONFIRMED / OBSERVED / INFERRED / UNKNOWN / CONFLICT`. The system builds partial vehicles without pretending unknown data is known (§7). Do not fabricate certainty that the source data does not support.
- **Protocols are independent, pluggable layers.** UDS rides on ISO-TP/CAN; J1939 is implemented separately from UDS; new protocols must be addable without changing the core vehicle model (§9, §10).

## Domain glossary (needed to read the code intelligently)

- **UDS** — Unified Diagnostic Services (ISO 14229); request/response with services like `0x10` (session), `0x22` (ReadDataByIdentifier), `0x27` (SecurityAccess). **NRC** = Negative Response Code.
- **DoIP** — Diagnostics over IP (ISO 13400); TCP/UDP with routing activation, alive check, logical addresses.
- **ISO-TP** — ISO 15765-2 transport for multi-frame UDS over CAN (flow control).
- **DID / DTC** — Data Identifier / Diagnostic Trouble Code.
- **ODX / PDX** — diagnostic specification format / packaged ODX archive.
- **ARXML / DBC** — AUTOSAR XML / CAN database, used for topology and network config.
- **DPDU log** — diagnostic protocol log used to reconstruct observed diagnostic behavior.
- **J1939** — commercial-vehicle CAN protocol (PGN, SPN, source/dest address, DM messages).

## Working priorities

Build order follows the roadmap in README §21 (Phase 1 Core Diagnostic Engine → CAN → DoIP → ODX/PDX → Log Reconstruction → …). The **MVP** (§24) is intentionally narrow and is the right first target: import ODX/PDX or PCAP/CAN → build a Vehicle Model → run a virtual ECU speaking UDS over CAN and DoIP → connect an external tester → capture a trace → inject a fault → reconstruct/update the same ECU from a trace.
