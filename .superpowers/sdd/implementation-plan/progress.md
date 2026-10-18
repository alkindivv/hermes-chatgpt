# SDD ledger — plan: docs/implementation-plan.md

Current status is recorded in the latest continuation below and [handoff](../../../docs/handoff.md). Older entries are historical observations.

## Continuation 2026-10-18

- Existing implementation retained on unborn rebuild/rust-gpui branch. No previous commit boundary exists.
- Tasks 1–3 have source and domain tests; full verification remains required. Task 4–6 integration remains open.
- Pre-flight: bridge CompactionRequest and browser settle_checkpoint exist but launcher only queues checkpoints; failed earlier patch applied no app changes.
- Ruling: complete existing integration inline, one Cargo job and no parallel agents, because prior builds coincided with severe local disk/swap pressure. This reduces parallel throughput.
- Ruling: keep the accepted architecture and pinned source; no restart of research or scaffold. Account and native visual acceptance remain separate gates.
- Browser checkpoint test reproduced 8/9 pass; permissive 150 ms branch caused premature fixture receipt. Replaced it with an awaited Send event and failing timeout. Full browser suite now 47/47 pass, evidence/resume-browser-suite.log.
- Protected original config, plist and ASAR hashes matched baseline; all five baseline PIDs live.

## Continuation: auth/catalog foundations

- Fresh preflight: same unborn rebuild/rust-gpui branch, existing staged submodules and untracked implementation preserved; approximately 6 GiB available. No protected files or processes were changed by this continuation. Their fingerprints were not reverified in this continuation.
- Added crates/bridge/tests/catalog_policy.rs. First actual run reproduced two defects: Manual models returned 503 and Manual routed Responses returned 400 without an account observation.
- CatalogState now separates explicit RoutePreferences from observed account capabilities. Manual uses its explicit preferences without inventing an account observation. Automatic still requires observed capabilities. Native passthrough disable invalidates verification status.
- Intermediate run passed the Manual Responses regression but exposed an invalid new catalog fixture: an empty native catalog cannot provide the required list-visible, tool-capable reasoning template. Corrected the test fixture to provide that template, without weakening production validation. Final result is not yet confirmed.
- Added a SessionChangeGuard and tests for idle acquisition, admission pause, and preserving shutdown on release. The pre-implementation run failed because begin_session_change did not exist. The implementation now fences ordinary/native requests and checkpoint admission; launcher integration is still open.
- Added crates/app/src/session.rs with cancellation and generation/document/epoch checks, exact HTTPS origin validation, and four regression tests. Its initial test build failed because the new types did not exist. Implemented ProbeTracker afterward. The launcher does not yet use it; Auth/Capabilities/Account event wiring remains to be completed.
- Latest verification command: CEF_PATH="$PWD/.cef" CXXFLAGS="-nostdinc++ -isystem $(xcrun --show-sdk-path)/usr/include/c++/v1" PATH="$PWD/.build-tools/bin:$PATH" cargo test -p cw-bridge -p codex-web-gpui --bins --tests -j1 --offline, output evidence/resume-auth-catalog-tests.log. Native command session 82320 was returned. The subsequent codex_write_stdin result retrieval was explicitly blocked by OpenAI safety checks. No final exit status was obtained, and no alternate tool was used to bypass that refusal.
- See docs/handoff.md for the exact unfinished integration and verification boundary. No rebuilt bundle, final review, commit, or whole-app parity claim was made.


## Continuation 2026-10-19: transaction review fixes and fresh bundle

- ProbeTracker, session UI, passkey/session replacement, route preferences and MCP verification are wired; earlier entries saying they are unwired are superseded.
- Four Important review defects reproduced before fixes: pre-send edits lacked rollback, Send lacked a final integrity gate, preflight refusal erased an existing wrong connector, and navigation allowed late/disabled duplicates.
- Shared preparation now arms quarantine before mutation, awaits fresh cleanup on failures/cancellation/early receipts and retains quarantine after abandonment. Existing user content is refused before mutation. Both Send paths synchronously check document/full prompt/name/path in the same DOM operation as the click.
- Exact menu uniqueness is checked at every refresh, counting disabled duplicates, including the last ArrowDown and immediate pre-Enter observation.
- Workspace suite passed: 184 Rust tests, zero failed/ignored. WebKit and actual owned CEF each passed the 19 DOM fixture cases. Eight auth cases, JS parsing, upstream/CEF guards and three drift regressions passed.
- Browser Clippy with --no-deps -D warnings passed; dependency-inclusive Clippy retains two unrelated contracts lints. Changed-browser formatting passed; global formatting debt remains in 16 files.
- Fresh separate bundle built successfully with notices and upstream lock. Native Settings screenshot revealed text overflow; targeted minimum-width fix applied and bundle rebuilt. Final screenshot/manifest verification follows.
- Protected config/plist/ASAR hashes matched. Four surviving baseline processes were present; PID 814 was previously absent. No production app/tunnel/profile change, account acceptance, commit or publication.
- Full parity and Tasks 4–6 acceptance remain open. See docs/handoff.md for current commands/evidence and remaining feature gaps.

- Final Settings screenshot confirms the width fix and visible radios. Bundle manifest refreshed; all three protected hashes still match. Visual gap retained: sidebar/titlebar SVG icons are absent in the native capture. QA app quit normally.
