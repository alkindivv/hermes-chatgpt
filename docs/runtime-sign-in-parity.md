# Isolated native sign-in parity

Status: native implementation with source/fixture verification only. Reference: `vendor/upstream` v6.1.10 (`8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`). Related runtime policies and remaining gaps: [runtime-parity.md](runtime-parity.md).

Read before implementation: `src/browser-login.ts`, `tests/browser-login.test.ts`, `launcher/electron/passkey-login-state.cjs`, its tests, `runtime.cjs` capture-marker validation and `browser-host.cjs` import flow. Existing runtime/lib/desktop exports were inspected and preserved. No prior sign-in module existed, so this was one assignment, not a duplicate worker implementation.

## Concrete caller API

Exports are available from `cw_runtime` and `cw_runtime::signin`:

```rust
SignInSession::start(profile: Profile, chrome: PathBuf, timeout: Duration)
    -> anyhow::Result<SignInSession>;
SignInSession::status(&mut self) -> anyhow::Result<SignInStatus>;
SignInSession::login_profile(&self) -> anyhow::Result<PathBuf>;
SignInSession::cancellation_handle(&self) -> SignInCancellation;
SignInCancellation::cancel(&self) -> anyhow::Result<()>;
SignInSession::continue_into(&mut self, destination: &OwnedLoginDestination)
    -> anyhow::Result<SignInReceipt>;
SignInSession::cancel(&mut self) -> anyhow::Result<()>;

OwnedLoginDestination::new(profile: Profile, relative: PathBuf)
    -> anyhow::Result<OwnedLoginDestination>;
OwnedLoginDestination::take(&self, now: DateTime<Utc>)
    -> anyhow::Result<ValidatedLoginState>;

validate_passkey_login_state(value: &serde_json::Value)
    -> anyhow::Result<ValidatedLoginState>;
validate_capture_marker(marker: &serde_json::Value, now: DateTime<Utc>)
    -> anyhow::Result<()>;
```

The caller must invoke `start` only after an explicit sign-in/passkey action. Neither `Profile`, `RuntimeFacade`, crate initialization nor application startup automatically starts sign-in. There is no default executable discovery, personal Chrome profile lookup, import from an original browser, or automatic account access. The executable must be an explicit absolute existing file; no shell/extra caller-supplied browser flags are accepted.

`SignInStatus` exposes only UUID, exact owned child PID, phase and optional CDP port. Phases: `Waiting`, `Capturing`, `Completed`, `Cancelled`, `Expired`, `Failed`, serialized in kebab case. `SignInReceipt` exposes transfer path, capture time, cookie/localStorage counts and `authenticated: false`. Cookie presence is capture evidence, not authenticated account/composer/capability evidence.

`continue_into` is a bounded **blocking** CDP workflow; put it on a worker or Tokio `spawn_blocking`, not the GPUI event thread. Retain the cancellation capability in UI and return the owned session even on an error:

```rust
// Explicit user action only. No such call with real Chrome was made during QA.
let mut login = SignInSession::start(profile.clone(), explicit_chrome, Duration::from_secs(600))?;
let cancel = login.cancellation_handle();
let destination = OwnedLoginDestination::new(
    profile.clone(), "browser/login-transfer-unique.json".into(),
)?;
// On a later explicit Continue action:
let task = tokio::task::spawn_blocking(move || {
    let result = login.continue_into(&destination);
    (login, destination, result)
});
// A UI Cancel action can call cancel.cancel() while the worker owns login.
// Return/retain the session handle until completion, or dropping it cancels/cleans it.
```

Destination must be an explicitly selected relative `.json` path beneath the **same Profile's** `browser/`. Traversal, symlinks, absolute paths and other-profile destinations fail closed. Existing files are not replaced, including an occupied path detected again under the destination lock. The runtime writes one private atomic `{marker, storageState}` envelope; it does not mutate a live CEF session. `take` locks, size-checks, revalidates fresh capture evidence and allowlisted state, removes the successfully consumed envelope and returns sensitive validated state. Replays through a second `take` fail because the file is gone. A nonsensitive private lock file may remain.

`ValidatedLoginState::storage_state()` returns filtered Playwright-shaped cookies/origins; `portable_state()` returns upstream Electron-style cookies/localStorage. These values contain secrets and must only be applied to a caller-owned browser session, never logged. `Debug` is explicitly redacted. The caller still owns destination CEF/tab exclusivity, clearing only its own prior state, applying cookies/localStorage, real authenticated Temporary Chat verification, capability detection, and failed-import cleanup. No runtime method claims those later browser steps succeeded.

## Owned lifecycle and capture

1. Create a new UUID directory `signin/<UUID>/chrome` under the selected owned Profile, with private directory permissions. Never reuse an existing login directory. Spawn and retain the exact native Child with owned HOME/CODEX_HOME/cwd, ignored stdin/stdout/stderr, `--user-data-dir=<new-directory>`, normal `--new-window`, background-mode/first-run/default-browser suppression, and Temporary Chat URL `https://chatgpt.com/?temporary-chat=true`. No automation, mock-Keychain or basic-password-store switch is injected.
2. Add `--remote-debugging-address=127.0.0.1 --remote-debugging-port=0`; Chrome chooses a new port and writes `DevToolsActivePort` inside that new profile. Read only this bounded, symlink-checked file, requiring a nonzero numeric port and `/devtools/browser/<UUID>` identity. Never attach to a caller-supplied endpoint, production port or pre-existing process. Browser traffic is not inspected before Continue.
3. A dedicated watchdog waits for the caller's positive timeout, at most ten minutes. On expiry it stops/waits the exact held child and removes only this UUID directory. Cancel does the same and wakes the watchdog. Early child exit is reported as failure when polled; status cleans up. Drop cancels the timer, waits its thread, stops the owned direct child and removes only that UUID subtree. The cloneable cancellation capability cannot adopt a PID or access a different browser/profile.
4. On explicit Continue, connect to that owned browser's CDP WebSocket, request `Browser.close`, allow up to five seconds for graceful exit and fall back to stopping only its held Child. Delete only that UUID profile's `Default/Sessions` and `Current Session`, `Current Tabs`, `Last Session`, `Last Tabs` records, matching the upstream offline capture preparation.
5. Relaunch the same disposable profile using another held native Child, headless with background networking disabled, `--restore-last-session` and `about:blank`; request a fresh CDP port and reject reuse of the initial port. Refuse offline capture if there is no page or a restored page is nonblank. The implementation fails rather than guessing or attaching to another browser.
6. Attach only to the owned blank page, enable page networking controls, set offline conditions, bypass service workers, stop existing workers, disable their event domain and intercept all requests. Fulfill paused requests with an empty local HTML response. Navigate to the Temporary Chat URL and verify the root frame is exactly the ChatGPT HTTPS origin before reading DOMStorage. Read cookies through the owned browser-level `Storage.getCookies`; default an omitted CDP SameSite to Lax, as Chromium/Playwright storage conversion does. CDP errors/events are never logged with payloads.
7. Sanitize/validate, close/reap offline Chrome, remove its UUID directory and stop the watchdog before writing the explicit destination envelope. There is no success marker if capture fails. Capture completion never sets `authenticated: true`, never infers Sol/Pro/Extra High capability and never reads another browser's database or Keychain directly.

The browser port is loopback-only; WebSocket messages/frames are capped at 16 MiB, endpoint files at 1 KiB, each socket operation at five seconds or the remaining deadline, and individual commands at 10,000 events. Fetch interception events are handled while matching command IDs; requests/results are not exposed as diagnostic strings. Transport uses the already cached/pinned `tungstenite 0.28.0` (one new direct dependency, no downloads). This avoids an ad hoc WebSocket/handshake implementation. No Node, Bun, Playwright runtime, external websocket tool or new build target is used.

## Validation source mapping

- At most 4096 cookies, 128 origins, 4096 ChatGPT localStorage entries in total; individual strings bounded to 2 Mi UTF-16 units, matching upstream JavaScript length semantics.
- Drop partitioned cookies, third-party identity-provider cookies, lookalikes, malformed authority/userinfo/port domains, repeated leading dots and non-ChatGPT/OpenAI domains. Keep only `chatgpt.com`, `openai.com` and their valid subdomains. Domain filtering combines upstream browser-login sanitizer and passkey validator constraints.
- Allowed cookies require nonempty names/domains/paths, slash-prefixed paths without controls, `?` or `#`, boolean secure/httpOnly, finite expiry and exact Strict/Lax/None SameSite. Unknown fields are not transferred. Portable SameSite maps to strict/lax/no_restriction; only positive expiries become expirationDate; host-only cookies omit the portable domain field.
- Keep localStorage only for exact `https://chatgpt.com`, validating every name/value and aggregate limits. Require at least one retained ChatGPT/OpenAI cookie. Sanitized state must fit 16 MiB. The source-shaped capture marker requires version 1, captureComplete true, source isolated-normal-browser-profile and RFC3339 capture time no more than eleven minutes old or sixty seconds in the future. Marker cap is 64 KiB. The atomic envelope consumer caps the combined file to 16 MiB plus 64 KiB.

## Verification, boundaries and remaining gaps

Last command: `CARGO_TARGET_DIR=crates/runtime/target cargo test -p cw-runtime -j 1 --offline --locked` — **27 passed, 0 failed** (18 unit, 9 integration, 0 doc). Build 6.22 s; unit suite 2.86 s; integration suite 0.58 s. Runtime formatting was checked separately. Five sign-in tests exercise allowlist/host-only mapping and redacted Debug, malformed fields/limits, marker schema/freshness, complete normal-to-offline capture/one-shot owned transfer, cancellation/expiry/destination isolation and survival of a test-owned unrelated child.

The integration fixture is Rust compiled with rustc into a temp native executable. It validates launch args, writes only its supplied temporary DevToolsActivePort, uses distinct loopback ports, creates disposable tab-session files and exits on a fixture-owned close request. Local WebSocket CDP mock servers exercise actual transport, command/event matching, offline-before-cookie ordering, request fulfillment and filtered capture. Moving the owned session to another thread is tested. No actual Chrome binary was invoked; no account, real cookies, personal/original profile, credential store, original app, Codex config, tunnel or VPS was read or changed. Temporary payload strings are fabricated fixture-only values. No screenshot, Accessibility or account sign-in proof exists.

Real Chrome/macOS passkey/Keychain behavior, Chrome's session-cookie restoration, exact CDP compatibility across Chrome versions, OS-level network isolation of the offline process, composer/account authentication, capability detection and destination CEF application remain unverified. Unlike upstream's normal-browser/no-debug-port then Playwright pipe capture, this native implementation uses fresh loopback CDP ports as requested; it does not inject --enable-automation. It is not a complete Playwright persistent-context policy port. Offline requests on the selected capture page are controlled and workers are stopped/bypassed, but this is not a system firewall or proof that all browser background network activity is impossible.

Filesystem isolation uses the existing runtime Profile checks and is not an adversarial same-user openat/O_NOFOLLOW sandbox. Cleanup operates on the generated UUID subtree; it does not sweep other sessions. Direct Child cleanup does not prove descendant process-tree termination or recover from a forcibly killed host. A process stuck in kernel termination may delay a blocking stop. Dropping a spawn_blocking JoinHandle does not stop its task: retain and invoke the explicit cancellation capability. A cancellation after a completed atomic transfer does not delete that already committed transfer; the caller can consume or explicitly discard its owned output. No original-journal/full runtime/application parity is claimed.

Only `crates/runtime/src/signin/**`, runtime exports, runtime Cargo dependency and this document were changed for this assignment. Existing lib/desktop/runtime functionality was preserved. Disk fell from about 3.4 GiB to about 514 MiB while parallel workspace work continued; the existing target and one job were reused, with no unrelated artifact cleanup or additional packaging/build target.
