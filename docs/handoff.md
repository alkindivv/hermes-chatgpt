---
created: 2026-10-18
updated: 2026-10-19
tags: [handoff, rust, gpui]
status: active
agent: codex
---

# Codex Web GPUI handoff

Continue [the specification](spec.md) and [implementation plan](implementation-plan.md). The existing Rust/GPUI/CEF implementation is retained. Whole-application parity remains incomplete.

## Checkout and protection

Repository: `/Users/alkindiwilagabrata/My Projects/Codex-Web-GPUI`; unborn branch `rebuild/rust-gpui`. Existing staged submodules and untracked work are preserved. No commit or publication occurred. Use one Cargo job, the existing target and `.cef` cache. Latest continuation began with approximately 4.3 GiB free.

Do not modify the original application, production Codex home/configuration, browser cookies, tunnel, VPS or login items. Development profiles stay inside the checkout or `~/.codex-web-gpui`.

## Integrated implementation

ProbeTracker is wired through `session_ui.rs` and launcher events. Tickets reject stale account/catalog observations. SessionChangeGuard fences bridge admission during session operations; route preferences are independent from account evidence. Explicit passkey Start/Continue/Cancel, owned session replacement and verified logout are implemented with fixtures. MCP verification requires runtime health and, in Automatic, exact connector selection plus cleanup. It explicitly does not claim a public tool roundtrip. Manual makes no automatic account/DOM probes.

## Review fixes

The four Important findings were reproduced and fixed:

- `browser/preparation.rs` owns the editor transaction shared by submit and checkpoints. Failed insert, prompt verification, attachment processing, cancellation and early checkpoint receipts await fresh-token cleanup. Unconfirmed cleanup and dropped futures retain quarantine. Initial user drafts, attachments and wrong connectors are refused before editor mutation.
- Both send paths pass the expected document, complete prompt, configured connector name and selected connector path. The page validates all of them synchronously in the operation that clicks Send, after attachment work.
- Connector preflight errors no longer clear a preexisting wrong pill. Maintenance verification only cleans its owned edits or an explicitly verified matching selection.
- Exact menu-name uniqueness counts disabled duplicates, every ArrowDown refresh and an immediate pre-Enter refresh. A transient duplicate on the last navigation iteration is also rejected.

The DOM serializer now preserves BR and paragraph line breaks while excluding connector pills. Attachment cleanup only removes recognized controls on owned filenames; unknown layouts remain unconfirmed.

## Verified evidence

- `evidence/review-workspace.log`: workspace command exited 0; 184 Rust tests passed, none failed/ignored. Includes 12 connector and 10 preparation regressions.
- `evidence/review-dom-green.log`: 19 actual WebKit DOM cases passed.
- `evidence/review-native-qa.json` and `review-cef-dom.log`: rebuilt isolated app launched, unique marker-bound owned CEF target identified through its child listener, health returned ok, and the same 19 DOM cases passed in actual CEF with a fixture-injected origin. Only loopback fixture URLs were loaded. The native process quit with exit 0. One helper was present immediately after exit and was absent on the subsequent read-only process check.
- `evidence/review-native-settings.png`: native Settings screenshot at 1120 x 720 points. Review found overflowing mode-description text. A targeted `min_w(0)` fix was added to the mode-picker text column, followed by a successful fresh bundle build. Final native screenshot `review-native-settings-final.png` confirms wrapped descriptions and visible radio controls; `review-native-layout-final.json` records a 1120 x 720 window and normal exit. An initial 3-second probe ran before a window was ready; the bounded window-readiness retry succeeded.
- `evidence/review-auth.log`: eight auth-response cases passed. All versioned browser JS assets parse.
- `review-upstream-check.log`, `review-cef-guard.log`, `review-upstream-tests.log`: pinned 321-file/54-method inventory and full CEF patch guard passed; three drift regressions passed.
- `evidence/review-browser-clippy-scoped.log`: browser all-targets Clippy with `--no-deps -D warnings` passed. The dependency-inclusive invocation stops on two preexisting collapsible-if lints in contracts/models.rs.
- Scoped rustfmt passed on changed browser files. Global format check still reports 16 previously unformatted files across app/bridge/browser session; these were preserved to avoid unrelated formatting changes.
- `evidence/review-bundle-final.log`: fresh separate bundle build exited 0, at `dist/codex-web-gpui.app`, ID `dev.alkindi.codex-web-gpui`. Packaging copies both license notices and upstream.lock.json. Final `review-bundle-manifest.json` confirms a changed layout binary, matching target executable, all resource hashes, CEF framework, five helper bundles and successful --version.
- `evidence/review-protected-before-bundle.json`: original config.toml, Info.plist and ASAR matched all baseline SHA256 values. PIDs 825, 831, 3655 and 3728 were present; baseline PID 814 was already absent.

Warnings remain: dependency block v0.1.6 future incompatibility and unused ProbeTracker::is_active in the app build. No current permission refusal occurred. Historical retrieval refusals in older ledger entries are not current test failures.

## Remaining work

Final bundle/layout and protected-file manifests are refreshed. Continue source-mapped gaps: Temporary Chat personalization recovery, Bigger Context staging, prediction/skill attachment effects, browser auto-approval, tunnel health/restart/migrations and application updates. Current persisted controls do not establish those runtime effects.

Real isolated account login/passkeys, attachments, native tool roundtrip, all six screens, motion/input/focus and performance acceptance remain open. Fixture DOM checks do not establish live ChatGPT DOM compatibility or whole-app equivalence. No authenticated session was borrowed and no production route was migrated.

Final read-only protection check: `evidence/review-protected-final.json` again matches all three protected hashes and observes the same four surviving baseline PIDs. The isolated QA app has exited. Native sidebar/titlebar SVG icons remain visually absent in the Settings capture and need a source/render investigation; do not claim visual parity.
