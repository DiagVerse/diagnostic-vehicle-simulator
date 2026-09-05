# ADR 0002 — Dynamic plugins vs. library crates

Status: accepted (Phase 2)

## Context

ADR 0001 established runtime dynamic-library plugins (via `abi_stable`) as the extensibility
mechanism. As we build the CAN/ISO-TP reconstruction pipeline, a question arises: should
*every* module in `engine/plugins/` (per the README's proposed tree) actually be a
dynamically-loaded `cdylib`?

Making everything a `cdylib` has real costs:
- Only `abi_stable`-safe types cross the boundary, so tightly-coupled building blocks (frame
  types, transport codecs) would need FFI mirrors of native types and constant conversion.
- Dynamic dispatch and copies on hot paths (per-frame) hurt performance.
- It provides no plug-and-play benefit for code that the engine always needs.

## Decision

Split modules into two categories:

**Dynamic plugins (`cdylib`, loaded at runtime via the stable ABI).** Used where the boundary
is a coarse-grained pure function and the module is a genuine user-facing extension point:
- **Application protocols** — e.g. `uds` (already a plugin): `(request, ecuSnapshot) -> (response, stateChanges)`.
- **Populators** — e.g. `canlog`, `pcap`, `odx`, `dpdu`: `(inputPath) -> VehicleModel`.

These are the two things the README says the platform must be open to ("protocols addable
without changing the core model"; "one Vehicle Model, many populators").

**Library crates (`rlib`, linked at compile time).** Used for shared codec/transport building
blocks that plugins and the runtime both consume and that are not independent extension
points:
- `can` — CAN/CAN-FD frame types.
- `isotp` — ISO 15765-2 segmentation/reassembly.
- (future) `uds-codec`, `doip-codec` — message decoders shared by populators and the runtime.

A populator `cdylib` links these `rlib`s statically and exposes only the coarse Populator ABI.

## Consequences

- The OSI layering from ADR 0001 still holds, but a "layer" may be a library crate rather than
  a `cdylib`. The directory layout groups them the same way; the crate type differs.
- Plug-and-play still applies where it matters: dropping a new protocol or populator library
  into `plugins.d/` extends the system without recompiling the core.
- Reconstruction Phase 2 is first implemented as library crates (`can`, `isotp`, `reconstruct`)
  proven by tests and a CLI; the `canlog` **Populator plugin** then wraps the `reconstruct`
  library behind the stable ABI. Getting the logic correct first, then wrapping it, keeps ABI
  churn low — the same "make it work, then make it pluggable" approach used before.
- If a building block later needs to be swappable at runtime, it can grow its own plugin ABI
  without changing callers that use the library directly.
