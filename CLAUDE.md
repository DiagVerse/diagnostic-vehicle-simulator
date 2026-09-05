# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

Implementation has started. **Phase 0 (bootstrap) and Phase 1 (core diagnostic engine) are complete.** The repo is under git (remote `DiagVerse/diagnostic-vehicle-simulator`), with a Rust engine workspace, a Vite/React/TypeScript UI, a working runtime plugin system, and CI. The build plan lives in `/Users/sri/.claude/plans/go-through-the-readme-md-transient-dongarra.md`.

Phase 1 delivered: the Unified Vehicle Model (`core-domain`), a UDS (ISO 14229) protocol as a dynamically-loaded plugin (`plugins/protocols/l7-application/uds`), a stateful virtual ECU runtime (`crates/ecu`), and the `ProtocolHandler` port bridging the two (`application`). `dvsim demo` runs a full diagnostic session through the dynamically-loaded UDS plugin.

Phase 2 (core) delivered: `crates/can` (CAN/CAN-FD frame types), `crates/isotp` (ISO 15765-2 reassembly), and `crates/reconstruct` (CAN log → Unified Vehicle Model). `dvsim reconstruct <canlog>` parses Vector `.asc`/candump logs and prints the reconstructed model; a golden + round-trip test proves the reconstructed ECU reproduces the log's observed responses. See ADR 0002 (plugins vs library crates) and 0003 (reconstruction pipeline). Deferred to Phase 2b: the `canlog` **Populator plugin** wrapper (currently a library + CLI) and a live virtual CAN bus + ISO-TP segmentation for external testers. Also: a dev diagnostics API/UI (`/ecu/*`) and `./scripts/dev.sh` run engine + UI together. Next: Phase 2b, then Phase 3 (DoIP + external tester).

**MVP-1 delivered** (`feat/mvp-load-simulate`): the engine holds a loaded vehicle and answers UDS **routed by CAN request address**. `core-domain` carries a `CanAddress` per ECU (request/response/functional ids, addressing mode, confidence); `crates/reconstruct` records it during correlation; the new `crates/simulation` holds the loaded `Vehicle` plus one stateful `VirtualEcu` per request id and routes physically, functionally (broadcast, with ISO 14229-1 Table 5/7 NRC suppression), or not at all (silence). `crates/api` exposes `POST /simulation/load`, `GET /simulation/state`, `POST /simulation/request`, `POST /simulation/reset`. See ADR 0004. Next: MVP-2 (UI load + simulate + timing parameters), MVP-3 (ISO-TP segmentation + SLCAN), MVP-4 (demo polish).

Deferred from the MVP-1 CAN/UDS review (recorded in ADR 0004, worth a `fix/*` PR): ISO-TP messages are ordered by their first frame rather than their last; consecutive-frame sequence numbers are not validated; the Vector `.asc` `x` extended-frame marker is discarded; `m_u16LogicalAddress` still stands in as an 11-bit ECU's response id; and the `~/.claude/can-analyzer/reference/` log format is not parseable yet.

Known cosmetic item: the raw Vehicle model JSON uses `mStrName`-style keys (Hungarian field names + serde camelCase); UI-facing DTOs use clean camelCase. Revisit with explicit serde renames if the model JSON becomes user-facing.

- `README.md` is the authoritative source for *intended* behavior, scope, and long-term architecture. Its `Project Structure` tree (§20) is a historical proposal — the **actual** layout is the hexagonal/OSI structure described below, not that tree.
- The engine lives in `engine/` (Cargo workspace); the UI in `ui/`; runtime plugins are dropped into `plugins.d/`. Architecture decisions are recorded in `docs/architecture/`.

## Build, lint, test commands

Rust needs `source "$HOME/.cargo/env"` in a fresh shell before `cargo` is on PATH.

Engine (run from `engine/`):
- Build: `cargo build --all`
- Test: `cargo test --all` (single test: `cargo test -p <crate> <test_name>`)
- Format: `cargo fmt --all` (check-only: `cargo fmt --all --check`)
- Lint: `cargo clippy --all-targets -- -D warnings`
- Run engine: `cargo run -p app -- serve` (defaults to `127.0.0.1:8080`, reads `plugins.d/`)

Plugins are `cdylib`s: after `cargo build`, copy `engine/target/debug/lib<name>.{dylib,so,dll}` into `plugins.d/` for the engine to load them at startup.

UI (run from `ui/`):
- Install: `npm install`
- Dev server: `npm run dev` (proxies `/health`, `/plugins` to the engine on :8080)
- Lint: `npm run lint`
- Build + typecheck: `npm run build`

CI (`.github/workflows/ci.yml`) runs all of the above on every push; **do not commit red**.

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

---

# Coding & Engineering Guidelines (Rust + JavaScript)

> These are the mandatory coding, naming, architecture, debugging, logging, error-handling, and implementation conventions for this project. **Project conventions take precedence over personal/idiomatic preferences**, but never at the cost of correctness.

## Toolchain reconciliation (READ FIRST)

The project naming convention below (Hungarian-style prefixes like `m_iRetryCount`; PascalCase function names like `DetectEcu`) **conflicts with Rust's built-in lints** `non_snake_case` and `non_upper_case_globals`. Because CI runs `cargo clippy --all-targets -- -D warnings`, Rust code using this convention **requires** silencing those lints at the crate root:

```rust
#![allow(non_snake_case, non_upper_case_globals)]
```

Add that to every engine crate's `lib.rs`/`main.rs` that uses the prefix convention. Do **not** remove `-D warnings` from CI — keep every other lint enforced. JavaScript/TypeScript has no equivalent conflict. Type/struct/enum/trait names remain `PascalCase` (already idiomatic Rust), so no allow is needed for them.

**Two settled decisions:**
- **Scope — going forward only.** The convention applies to all code written from **Phase 1 onward**. The small Phase 0 scaffold (`core-domain`, `plugin-contract`, `application`, `api`, `app`, `sample-plugin`) was written before the convention and is intentionally left in idiomatic snake_case — do **not** mass-rename it. When materially editing a Phase 0 file later, migrate the touched code to the convention; don't churn untouched code.
- **Serialized boundary structs use serde rename.** Structs that cross to JS/JSON use Hungarian field names in Rust plus `#[serde(rename_all = "camelCase")]` (or explicit `#[serde(rename = "...")]`) so the wire/JS side sees clean camelCase (`serviceId`, `transactionId`). Internal (non-serialized) types just use the Hungarian names directly.

## 1. Golden Rules

1. Readability over cleverness.
2. Explicit control flow over compressed logic.
3. Meaningful names over short names.
4. One responsibility per function.
5. Preserve existing behavior when modifying code unless a behavior change is explicitly requested.
6. Reuse existing architecture before creating new abstractions.
7. Handle errors explicitly.
8. Log important state transitions and failures.
9. Never hide important business logic inside clever helpers.
10. Do not introduce unnecessary design patterns.
11. Do not refactor unrelated code.
12. Build and test after meaningful changes.
13. Code must be understandable without the original author.
14. Assume a future developer will debug from logs without a debugger.

Primary goal: **expert-level engineering decisions + mid-level (2–4 yrs) developer readability.** Optimize for clarity, predictability, debuggability, maintainability, consistency, explicit behavior, and safe error handling — not minimum line count.

## 2. Naming Convention

Format: `[scope prefix]_[data-type prefix][DescriptiveName]` (PascalCase descriptive portion). Examples: `g_iRetryCount`, `m_iRetryCount`, `c_iMaxRetryCount`, `s_iInstanceCount`, local `iRetryCount`.

Scope prefixes:

| Scope | Prefix | Example |
|---|---|---|
| Global variable | `g_` | `g_iActiveConnectionCount` |
| Member/instance | `m_` | `m_iRetryCount` |
| Static member | `s_` | `s_iInstanceCount` |
| Constant | `c_` | `c_iMaxRetryCount` |
| Local variable | none | `iRetryCount` |
| Function parameter | none | `iTimeoutMs` |

Do not use `g_` for values that are not truly global. Avoid mutable global state; prefer controlled ownership through structs/modules.

Data-type prefixes — primitives: `b` bool, `i` int, `u` unsigned, `i64`/`u64`, `f` float, `d` double, `c` char, `str` string, `by` byte. Collections: `vec`, `arr`, `map`, `set`, `queue`, `stack`. Rust-specific: `opt` `Option`, `res` `Result`, `arc` `Arc`, `mtx` `Mutex`, `rw` `RwLock`, `tx`/`rx` channel ends, `task` future/handle.

Do not mechanically encode every generic wrapper if it makes names unreadable — the prefix must aid debugging, not bloat the name.

Booleans read as a question: `m_bIsConnected`, `m_bHasResponse`, `m_bCanRetry` — not `m_bStatus`/`m_bFlag`.

Functions describe an action: `InitializeConnection`, `DetectEcu`, `SendDiagnosticRequest`, `HandleResponsePending` — not `DoStuff`/`Process`/`Handle`. Structs/enums/traits use descriptive domain names: `ConnectionManager`, `DiagnosticRequest`, `EcuState`, `CommunicationProvider`. Accepted automotive abbreviations: ECU, CAN, UDS, DoIP, DTC, NRC, DID, SID, VIN, OBD.

## 3. Readability Over Cleverness

Avoid: multiple logical ops per line, long expressions, nested ternaries, clever iterator chains that hide behavior, excessive closures, unnecessary generics, complex macros, code-golf, excessive abstraction. Prefer: explicit control flow, clear `if/else`, one logical operation per statement, named intermediate values, small focused functions. Longer code is acceptable when it makes behavior easier to understand.

## 4. Rust Rules

- **Ownership/borrowing:** prefer borrowing (`&str`, `&[u8]`, `&Configuration`) when only reading. Every non-trivial `clone()` needs a clear reason; do not clone to silence the borrow checker.
- **`unwrap()`/`expect()`:** do not use casually in production code. Prefer `?` or explicit `match`. `expect()` is allowed only for a genuinely-unviolatable invariant, and its message must explain that invariant.
- **Concurrency:** must be explicit and justified. Use the simplest suitable mechanism (async/await, channels, `Arc`, `Mutex`, `RwLock`, atomics). For shared mutable state, document owner, who mutates, why sync is needed, and locking/deadlock expectations. Never hold a lock across a slow operation.
- **Async:** no blocking calls in async paths unless isolated. Keep async flow as explicit sequential stages, not deeply nested expressions.

## 5. Error Handling

- Prefer typed errors (enums like `CommunicationError { Timeout, ConnectionFailed, InvalidResponse, TransportFailure }`).
- Errors must carry enough context to debug (ECU, Service, DID, TimeoutMs, …), never bare "Failed"/"Error".
- Never discard errors silently (`let _ = send_request();`) unless intentional and documented.
- Add context before returning when direct propagation loses information.
- JavaScript: use `async/await` with `try/catch`; log context on failure; never swallow errors with an empty `catch`.

## 6. Rust ↔ JavaScript Boundary

Make the boundary explicit with clear request/response/error/event models, serialization format, and validation. Prefer named domain objects over loose anonymous objects. The same semantic names must be recognizable on both sides (e.g. `serviceId`, `isExtendedAddressing`, `timeoutMs`, `transactionId`).

## 7. Logging (first-class debugging feature)

Assume a support engineer has only the log file. Logs should answer: what operation, which component, which ECU/device, what request/service, what response, what state, what failed and why, was a retry attempted, what happened next.

Levels: TRACE / DEBUG / INFO / WARN / ERROR / FATAL (use the existing `tracing` framework — do not add another without approval). Include context where applicable: timestamp, level, component, operation, `TransactionId`, EcuId, ServiceId, DID, NRC, ErrorCode, State, RetryCount. Never log secrets (passwords, tokens, API keys, private keys, session credentials).

**Transaction/correlation IDs** (e.g. `TXN-20260905-000123`) must flow through the entire chain JS → Rust API → managers → ECU → response → back, so one operation is reconstructable from a large log. Log important **state transitions** explicitly; do not flood logs with noise.

## 8. Automotive Diagnostic Flow (keep explicit)

Make these stages individually visible in code — never compressed into one expression: ECU detection → validation → request/service execution → positive response → negative response/NRC → Response Pending (NRC `0x78`) → timeout → communication error → retry → next ECU/service.

- **Required vs optional ECU:** required-ECU detection failure triggers failure handling; optional-ECU absence skips its services and continues. Do not hide required/optional behavior inside complex booleans.
- **UDS/NRC:** explicitly distinguish positive, negative, NRC `0x78`/pending, timeout, communication failure, unexpected, invalid.
- **Retries are intentional:** define which failures are retryable (timeout/transport → retry; NRC `0x78` → wait/process pending; invalid/unsupported/invalid-request → do not retry). Log RetryCount, MaxRetryCount, reason, TransactionId, ECU, service.

## 9. Comments, Constants, Enums

Comments explain **why**, not what; do not comment obvious code. Public APIs / complex modules / non-obvious behavior get doc comments (purpose, inputs, outputs, errors, ownership/lifetime, thread-safety, protocol assumptions, side effects). No magic numbers — use named constants (`const c_iMaxRetryCount: u32 = 3;`, `const c_u8ResponsePendingNrc: u8 = 0x78;`). Prefer enums for finite states over integer codes.

## 10. Architecture & Change Discipline

- **Avoid over-engineering:** no patterns/abstractions/generics/macros/wrappers without a real requirement. Use the simplest design that fits the existing architecture.
- **Preserve architecture:** before creating any new struct/trait/manager/service/helper/module, search for an existing one that solves it; do not duplicate functionality.
- **Modifying existing code:** preserve behavior, keep changes focused, follow existing logging/error-handling/concurrency/APIs, do not refactor unrelated code or rewrite a whole module for a localized change.
- **Separation of responsibility:** UI → Application Service → Diagnostic Manager → Communication Manager → Transport. UI must not contain protocol logic; the communication layer must not contain UI behavior.

## 11. Testing & Validation

Test behavior (not incidental implementation detail) with descriptive names (`test_diagnostic_response_pending`, `test_no_retry_on_invalid_request`). Cover failure paths, not just happy paths. Priorities: core business logic, diagnostic protocol logic, error handling, state transitions, retry behavior, Rust↔JS communication, config validation, boundary conditions.

After meaningful changes: format → build → run relevant tests → check warnings → review logs for new errors → review the diff. Use the project's configured Cargo/npm commands (see "Build, lint, test commands" above). Do not bump dependency versions to make an unrelated build pass unless explicitly requested.

## 12. Priority When Rules Conflict

1. Correctness and safety
2. Explicit user requirements
3. Existing project architecture
4. Existing public API compatibility
5. Error handling and debuggability
6. Project naming conventions
7. Readability
8. Performance optimization

Do not sacrifice correctness to satisfy a naming convention.

## 13. What NOT to do

Do not: rewrite unrelated modules; mass-rename existing variables unless requested; introduce a framework/logging system without approval; change public APIs or dependency versions unnecessarily; add abstractions merely because they look cleaner; hide errors; use `unwrap()` casually; add excessive comments/logs; optimize prematurely; shorten readable code just to cut lines; modify behavior outside the requested scope.

> **Do not write code to impress with cleverness. Write code another developer can understand, debug, modify, test, review, and support years from now.**
