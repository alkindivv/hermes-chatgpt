# Codex Web GPT: Rust + GPUI reconstruction

## Requested outcome

Rebuild `/Applications/Codex Web GPT.app` using Rust and GPUI. Preserve the original functions, state, flows, interface, and maintenance relationship with `miuuyy/codex-chatgpt-web`. Do not disturb any running launcher, Codex task, VPS tunnel, production browser profile, or installed integration.

## Reference identity

- App version: 6.1.10, bundle identifier `dev.codexwebgpt.launcher`.
- Upstream tag: `v6.1.10`, commit `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`.
- Installed ASAR SHA-256: `3ff95d35397afe403474fdfc185b18ae6526085e291f3fa26b0c1d6ac181daac`.
- REA parent evidence: `ev_54c8f20f027f43d6b58415147f113eab0797d8f1fd3c41f533bad59e946d9a49`.
- REA summary evidence: `ev_0208177d9ab29157ed46ce166ed877112956b91f1a6b3e65e746bbc5ed307be2`.
- REA statically identified 54 exposed API members. Its dependency graph is partial because expanded third-party relationships exceeded its resource budget. Static evidence is not runtime parity.

## Architecture

Use a Cargo workspace with a native GPUI launcher, CEF browser, Rust state/configuration and policy, Rust Responses/MCP protocol processing, and owned runtime supervision. The ChatGPT website necessarily executes browser JavaScript. Browser DOM scripts derived from upstream remain versioned assets; they are not an Electron launcher or a Bun application runtime.

The six launcher surfaces are Browser, Setup, MCP, Activity, Limits, and Settings. Preserve onboarding stages, 252 px resizable sidebar, 46 px titlebar, original dark tokens, fonts available on macOS, controls, and state feedback. Use the actual upstream copy and vector assets where licensed. Preserve MIT attribution. Do not invent a successful sign-in, smoke test, connector, model capability, tunnel, or completed turn.

All application-owned storage defaults to `~/.codex-web-gpui`, including a separate `codex-home` and Chromium profile. All writable paths and listeners must be bound to that profile. Do not import cookies, change `~/.codex/config.toml`, replace `/Applications/Codex Web GPT.app`, use production ports, install login items, stop another process, or contact a VPS as part of development verification. Route installation is confined to this application's selected isolated Codex home. A production route migration is a separate operational action.

## Required parity domains

1. Launcher state, localization, onboarding, settings, preference persistence, navigation, sidebar sizing, window controls, logging, error/operation/update feedback.
2. Browser persistence, account sign-in/out, native navigation and zoom, up to five task-bound browser surfaces, task/epoch ownership, cancellation, focus, hidden turns, model/effort verification and attachment acceptance.
3. Automatic and Zero Risk modes; manual confirmation deadlines, prompt copying, separate connector identities, retained and fresh conversations, saved chats, compaction and Bigger Context.
4. Responses parsing and SSE, native model passthrough, exact routed model catalog, native tool/broker capabilities, MCP discovery/dispatch, native compaction/control semantics, cancellation and drain behavior.
5. Integration journals and restoration, supervision, tunnel setup/checksums, credentials isolation, diagnostics, autostart, resource installation and updates.
6. Local limit estimates with duplicate-receipt protection, rolling periods, account scope and uncertain sends.
7. Upstream commit pinning, file/method inventories, explicit drift detection, feature-to-source mapping and regression checks before accepting an update.

## Verification boundaries

Compile, focused behavioral tests, fixture/contract comparisons, native launch and screenshots must each have their own evidence. No test count substitutes for browser sign-in, a real account turn, full MCP operation, or visual comparison. Account-bound acceptance requires an authenticated isolated profile and is reported separately if unavailable. A partial port must remain explicitly incomplete in the README, parity ledger and final report.

## Documentation reviewed

Read the upstream README, architecture and relevant source before implementation. Context7 was used for official Rust process ownership documentation (`/rust-lang/rust`) and GPUI application/render/context documentation (`/websites/gpui_rs`). The installed Rust compiler is 1.99.1. The selected CEF integration reference is `bokuweb/gpui-cef` at `1e8870069d8765d7aeb6cc74f1026bd7bca4aed2`; its documented input, multiple-view and copying limitations must be tested, not assumed solved.
