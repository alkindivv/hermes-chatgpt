# Codex Web GPT GPUI Implementation Plan

**Goal:** Reconstruct the reference application in native Rust and GPUI without affecting the active installation.

**Architecture:** Separate pure reference contracts, the Responses/MCP bridge, owned system runtime, and GPUI/CEF application. Keep upstream reference code unmodified and generate a source-bound drift ledger.

**Tech Stack:** Rust, GPUI, CEF 151, serde, tokio, HTTP/SSE and JSON-RPC.

**Spec:** `docs/spec.md`.

## Global constraints

- Baseline upstream SHA `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5` (6.1.10).
- Development writes only to this checkout and `~/.codex-web-gpui`.
- Do not modify, restart, replace or take ownership of the existing app, route or tunnel.
- Preserve exact source behavior; report unmapped features and untested account flows.
- Limit compilation to one concurrent job. Do not launch upstream setup or service commands.

## Review focus

- A conflicting active route must never be overwritten.
- A sixth concurrent browser turn must fail without disturbing an existing owner.
- A repeated accepted send or submission receipt must not be replayed or counted twice.
- A stale turn capability must not dispatch a local tool or cross task ownership.
- An upstream API addition must fail the drift check until reviewed and mapped.

## Task 1: Contracts and drift inventory

Files: `crates/contracts`, `tools/upstream.py`, `upstream.lock.json`, `docs/parity.md`.

- [ ] Read upstream launcher state, model policy, limit policy and related tests.
- [ ] Write failing behavioral tests for state defaults, clamping, model selection and rolling receipts.
- [ ] Implement serializable Rust types and source-compatible policy.
- [ ] Generate an exact file/hash/API inventory; compare updates without modifying the pinned reference.
- [ ] Run the contract tests and test deliberate drift detection.

## Task 2: Responses and MCP protocol

Files: `crates/bridge`.

- [ ] Read upstream Responses parser, prompt compiler, broker, server, MCP and cancellation contracts.
- [ ] Write failing tests for literal role preservation, image extraction, SSE completion, owner isolation and cancelled/expired capabilities.
- [ ] Implement typed request/event processing, turn registry and loopback HTTP/MCP dispatch with an explicit browser interface.
- [ ] Verify local fixtures and in-process integration tests; do not connect production services.

## Task 3: Owned runtime and integration

Files: `crates/runtime`.

- [ ] Read upstream integration journal, profile, process, supervisor and tunnel contracts.
- [ ] Write failing tests for profile boundaries, journal conflict/restore and non-idle drain refusal.
- [ ] Implement isolated configuration, atomic persistence, owned child lifecycle, diagnostics and native command entrypoints.
- [ ] Verify all filesystem effects in temporary directories and all process effects on test-owned children.

## Task 4: GPUI launcher and CEF browser

Files: `crates/app`, `assets`, `vendor/gpui-cef`.

- [ ] Extract reference layout, copy, icons, browser ownership and event flows.
- [ ] Implement GPUI navigation, onboarding, settings, activity, limits and MCP/setup surfaces with real state and operation feedback.
- [ ] Integrate persistent isolated Chromium, navigation, zoom and task-bound browser ownership.
- [ ] Connect native actions to Rust runtime/bridge; preserve unsupported capability errors instead of simulating success.
- [ ] Build and open only the separate application bundle; inspect native UI and browser behavior.

## Task 5: Browser execution and parity acceptance

- [ ] Wire reference DOM behavior to the native browser interface with explicit model/turn/attachment ownership.
- [ ] Exercise fixture requests, cancellation, concurrent ownership and compaction.
- [ ] Compare original/reference and native screenshots at matching window dimensions.
- [ ] Record authenticated tests separately; do not borrow a running browser session.

## Task 6: Packaging and final verification

- [ ] Build a separately named `.app` with pinned dependencies, upstream license and helper resources.
- [ ] Run workspace tests, formatting and source drift checks.
- [ ] Re-read protected configuration fingerprints and original process ownership.
- [ ] Update parity status, reproduction commands, session note and handoff with actual evidence.

## Execution

Perform independent contract, protocol and runtime work in disjoint directories. The primary worker owns the GPUI/CEF integration, shared workspace manifest and final integration. All contributors must list exact changed paths and unresolved parity gaps.
