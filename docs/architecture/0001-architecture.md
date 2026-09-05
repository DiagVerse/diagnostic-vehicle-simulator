# ADR 0001 — Overall architecture

Status: accepted (Phase 0)

## Context

We are building a reconstructable, executable virtual vehicle platform (see `README.md`).
It must be robust, cross-platform, realtime-grade, and highly modular, with a visually
polished UI. Key user requirements: modules must be plug-and-play, the protocol stack must
follow OSI layering, and UI must be strictly separated from business logic.

## Decision

**Language / runtime:** Rust + `tokio` for the engine; React + TypeScript + Vite + Tailwind
+ React Flow for the browser UI.

**Architecture style:** Hexagonal (ports & adapters).
- `core-domain` — pure business logic (the Unified Vehicle Model + rules). No I/O, no
  protocol or UI dependencies.
- `application` — use cases orchestrating the domain through *ports* (traits).
- Everything else (protocols, parsers, transports, SSH, serial, persistence, the HTTP API)
  is an *adapter* implementing a port.
- `ui/` is a separate project that only speaks the REST/WebSocket API — it cannot import
  engine code.

**Protocol stack:** strict OSI layering, one crate per layer under
`engine/plugins/protocols/l1-physical … l7-application`. Each layer only depends on the
trait of the layer below, so upper layers are transport-agnostic (e.g. UDS runs unchanged
over ISO-TP/CAN or TCP/DoIP).

**Plug-and-play:** runtime dynamic-library plugins. Each module compiles to a `cdylib` and
is discovered from `plugins.d/` at startup. Because Rust has no stable ABI (raw `libloading`
of Rust types is undefined behaviour), the plugin boundary uses **`abi_stable`** with a
versioned plugin contract (`plugin-contract` crate). The host checks the ABI/layout on load
and skips incompatible libraries.

## Consequences

- Adding/swapping a protocol, parser, or adapter = drop a compiled library into `plugins.d/`;
  no core rebuild.
- Only `abi_stable`-safe types may cross the plugin boundary; contract changes are deliberate
  and versioned.
- Plugins are built per target OS; the loader matches the host's library extension.
- FFI cost is kept off hot paths by crossing the boundary at coarse granularity (whole
  frames/PDUs, not per byte).
- If runtime loading ever proves too heavy, the same trait design collapses to compile-time
  linking with minimal change.
