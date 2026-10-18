---
created: 2026-10-18
updated: 2026-10-19
tags: [parity, gpui, session, mcp]
status: active
agent: codex
---

# Launcher session and connector integration

This supplements [browser parity](browser-parity.md), [runtime sign-in parity](runtime-sign-in-parity.md), and the [reconstruction specification](spec.md). It records integration rather than certifying whole-application parity.

## Session and catalog

`app/session.rs` keeps one cancellable profile inspection. Tickets bind the generation, request serial, tab identity, epoch, and document URL. Navigation, mode changes, account invalidation, cancellation, and tab closure reject stale results. `session_ui.rs` connects that controller to the launcher. A `SessionChangeGuard` blocks new bridge admission until a worker result and its cleanup status have been consumed.

Route preferences are separate from account evidence. Manual exposes Zero Risk, plus Zero Risk Pro only when explicitly enabled; it does not inspect account DOM. Automatic requires account/model evidence. Startup, installation, mode changes, and supported preference changes apply the current route policy to the bridge. Only an installed isolated route enables credential-transparent forwarding to the fixed official native endpoint. Persisted catalog flags do not replace a current authenticated native catalog fetch.

`SessionProbe` distinguishes confirmed sign-out from network failure or an invalid server response. The latter do not establish logout. The account key is derived from server identity; secret session payloads are not diagnostic output.

## Explicit passkey and logout

`session_login.rs` exposes Start, Continue, and Cancel. Chrome uses a new disposable profile, never the user's normal Chrome profile. Capture runs off the GPUI thread. Continue is accepted once; cancellation retains a direct capability for the owned Chrome process. A validated transfer envelope is consumed once before applying state to the owned CEF session.

Before replacement, the launcher parks its other views. Browser replacement clears owned cookies and allowlisted origin storage, applies only validated state, checks the main-frame origin before localStorage writes, then verifies server authentication and account/model capabilities. Cancellation after a write performs fresh cleanup. Failed cleanup holds admission and quarantines the page. Logout requires positive signed-out server evidence, not simply absence of a cookie or a failed network request.

This is fixture/source evidence. Real passkeys, account login, macOS Keychain behavior, Chrome restoration, and a live authenticated CEF transfer remain separate acceptance tests.

## Connector selection and MCP verification

The source reference is `browser-worker.ts`'s `selectConnector`, `selectedConnectorControl`, `attachedPromptText`, and `verifyConnectorExclusive`, together with `launcher/electron/main.cjs`'s MCP verification branch.

`browser/connector.rs` uses the source `@codex` mention with at most three complete trigger attempts. Observations must show one exact name. The real keyboard selection moves to that row before Enter. A new observation must then show the configured connector pill. Duplicate names, disabled choices, a wrong selected pill, document changes, or an existing user draft prevent success. Maintenance verification cleans the owned composer and never calls Send. Failed or cancelled edits need confirmed cleanup; uncertainty keeps the engine quarantined.

`TurnInput.connector_name` supplies the configured connector for automatic ordinary turns and automatic checkpoints. Smoke tests keep no connector because their source marker requires no tools. Prompt inspection excludes the connector pill rather than treating its label as prompt content.

`mcp_ui.rs` first checks fresh health for the held runtime instance. Automatic additionally requires exact connector selection and confirmed composer cleanup. Manual requires local health and explicitly retains manual connector selection. A ready result does not claim an actual public tool roundtrip. Cancelled or stale results cannot set `mcpSetupComplete`.

The native connector currently fails when Temporary Chat personalization prevents access; the full labeled/structural source recovery policy remains open. This limitation must not be hidden behind a success state.

## Evidence

- `evidence/connector-red.log`: missing connector implementation reproduced by the new transport cases.
- `evidence/connector-green.log`: connector selection/cleanup tests passed; the cancellation fixture was subsequently strengthened to await an actually pending observation.
- `evidence/mcp-ui-red.log`: health-only Automatic success and stopped-runtime Manual success were rejected by the new tests before implementation.
- `evidence/connector-integration.log`: app/browser Rust integration suite passed after wiring the controller.
- `tools/test-auth.cjs`: executes the actual auth asset against controlled server responses.
- `tools/test-connector-dom.swift`: disposable real WebKit DOM fixtures with an injected test origin. Fresh execution passed 19 cases in WebKit (`review-dom-green.log`). The same cases also passed in the separately rebuilt application’s owned CEF on a local fixture with an injected origin (`review-cef-dom.log`). This is DOM fixture evidence, not authenticated ChatGPT or native input parity.

See [handoff](handoff.md) for final workspace, guard, packaging, and review evidence. Do not infer current acceptance from a historical log filename.


## Transaction review follow-up

Shared `browser/preparation.rs` now rolls back owned prompt/pill/files on failed preparation and caller cancellation, and quarantines abandoned or unconfirmed operations. Preexisting text, attachments and wrong selections are refused before editing. Both ordinary and checkpoint Send operations validate document, complete prompt, exact connector name and the previously selected path synchronously before clicking. Menu duplicate checks include disabled choices and every keyboard refresh, including a final observation before Enter. See [handoff](handoff.md) for regression and packaging evidence.
