# Native runtime parity

Baseline: upstream v6.1.10, `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`. Scope: `crates/runtime/**` and this document. This is a partial native runtime; fixture tests do not prove an authenticated account, the whole application, or a public tunnel.

## GPUI integration API

```rust
use cw_runtime::{Profile, RuntimeFacade, Activity};
let profile = Profile::isolated(root)?; // absolute PathBuf
let mut runtime = RuntimeFacade::new(profile);

// Only from an explicit user action with newly supplied tunnel credentials.
let result = runtime.configure_install_start_mcp(
    tunnel_id, key_bytes, "http://127.0.0.1:YOUR_PORT/mcp",
    "automatic", "Codex GPUI Automatic",
).await?;
// result.ready means this owned child passed healthz, readyz AND MCP diagnostics.
// It is a timestamped observation, not a permanent connection guarantee.
let current = runtime.verify_mcp().await?; // status().await is an equivalent fresh probe
runtime.stop_mcp()?;

// Bridge HTTP and browser execution must hold separate guards until work is finished.
let guard = runtime.supervisor.gate.enter(Activity::Browser)?;
drop(guard);
runtime.shutdown()?; // refuses either nonzero counter; stops both owned children
```

`configure_install_start_mcp(&mut self, tunnel_id: &str, key: &[u8], endpoint: &str, mode: &str, name: &str) -> Result<McpSetupResult>` is async. Result fields: `ready`, `health_verified_at`, `pid`, `health_endpoint`, `profile_dir`, `profile_name`, `mode`, `connector_name`, `version`. No field invents a public connector URL or successful ChatGPT registration. `mode` accepts `automatic` / `manual`; the caller supplies the local MCP endpoint implementing that contract. The function cannot change the bridge's browser/tool policy by labeling an endpoint. The connector name is launcher metadata; this operation does not create a ChatGPT connector remotely.

For UI ownership transfer, always return the facade even when the operation fails:

```rust
let task = tokio::spawn(async move {
    let outcome = runtime.restart_saved_mcp().await;
    (runtime, outcome) // send both through the UI event; do not drop runtime on Err
});
```

Exact lifecycle signatures:

```rust
pub async fn verify_mcp(&mut self) -> anyhow::Result<serde_json::Value>;
pub async fn status(&mut self) -> anyhow::Result<serde_json::Value>;
pub async fn restart_saved_mcp(&mut self) -> anyhow::Result<McpSetupResult>;
pub fn mcp_status(&mut self) -> anyhow::Result<serde_json::Value>; // cached health + live child check
pub fn stop_mcp(&mut self) -> anyhow::Result<()>;
```

`verify_mcp`, `status` and the existing `check_mcp_health` all make fresh healthz/readyz/MCP-log probes for the held child. A newly constructed facade has no child and returns `{ready:false,state:"stopped"}`; it never treats the saved descriptor as connected. The host explicitly awaits `restart_saved_mcp()` on startup if reconnection is desired. No login item or external service is installed.

After successful readiness only, setup atomically saves version-1 `runtime/mcp-saved.json`. It contains a canonical UUID instance identifier, explicit tunnel ID, local endpoint, mode/name and profile/key/manifest digests, **not** literal credentials or a reusable PID. `restart_saved_mcp()` resolves only `tunnel/<UUID>/` beneath the selected owned Profile; validates the saved schema, platform/pinned binary manifest, exact profile/key bytes, key permissions and every bundled asset hash; checks native `--version`; clears the old owned health URL file; then starts a new held child and performs the same readiness probes. It uses only existing owned files and makes no release-download or key-import request. No arbitrary saved executable/profile path is accepted. Failure to validate occurs before stopping an existing held child.

`stop_mcp()` preserves this saved configuration. Orderly application exit/drop kills and waits the direct child; a new facade can then reconnect the same profile without copying an original application's files. An advisory per-instance owner lock prevents another facade/process from starting the same saved instance while it is held. An active-owner marker is removed only after direct-child cleanup. If the host exits abnormally and leaves that marker, restart **refuses** potential orphan duplication: automatic crash-orphan takeover is not implemented, and no persisted PID is adopted or killed. The normal-exit/reopen path is covered by tests; forced-host-crash recovery remains an explicit gap.

Setup performs these real operations:

1. Validate explicit key, tunnel ID, loopback HTTP MCP endpoint, mode and connector name; refuse a second currently owned MCP instance.
2. Download the exact platform archive and `SHA256SUMS.txt` from the official pinned `https://github.com/openai/tunnel-client/releases/download/v0.0.16`. HTTPS-only, 120 s timeout, no inherited proxy, 100 MiB archive / 1 MiB sums limits. Validate the unique exact asset checksum before extraction or execution.
3. Create a fresh UUID instance under this profile's `tunnel/`. Reject archive traversal, symlinks, duplicate basenames and more than 400 MiB total expanded data. Install regular assets, including bundled companions, only in that instance's `bin/`. Record archive, executable and all companion/regular asset hashes. Execute the installed **native** `--version` command with a 10 s timeout and bounded concurrently drained stdout/stderr; require exact version token `0.0.16`.
4. Write a 0600 fresh runtime key and selected named profile `<profile_name>.yaml`, under a 0700 owned instance. JSON syntax is used as a YAML 1.2 subset. The official v0.0.16 schema contains `config_version: 1`, `control_plane.tunnel_id`, `control_plane.api_key: file:<owned-key>`, `mcp.server_urls: [{channel: main, url: <explicit-endpoint>}]`, `health.listen_addr: 127.0.0.1:0`, `health.url_file: <owned-path>`, `admin_ui.open_browser: false`. Neither argv nor YAML contains the literal key.
5. Spawn and hold the actual foreground child with exact official flags: `run --profile-dir <owned-instance> --profile <selected-profile> --health.listen-addr 127.0.0.1:0 --health.url-file <owned-path>`. stdin/stdout/stderr are null; environment and cwd are owned. This intentionally uses the official foreground `run` path, rather than upstream's detached `runtimes connect`/PID adoption, to preserve Rust Child ownership. No shell, launchd, global profile lookup, unrelated alias cleanup, or PID adoption occurs.
6. Wait up to 120 s for the owned health URL file, require an explicit loopback HTTP origin, reject redirects, check the process is still alive, require successful `/healthz` and `/readyz`, then inspect `/api/logs?limit=100`. Match upstream's last-two-minutes internal 502 error predicate for MCP initialize/tools/call. A matching fatal event immediately rejects readiness. Missing diagnostics, non-success health, unknown endpoints and process exit cannot produce success. Error/cancellation drops the held child and kills/waits that direct child.

`verify_mcp().await`, `status().await` and `check_mcp_health().await` repeat the probes and changes `ready` to false on loss of health. `mcp_status()` and JSON `mcp.status` return the last observation plus process liveness; they do **not** perform network probes. GPUI should await `verify_mcp` or `status` before displaying a refreshed Connected status. `stop_mcp()` checks the shared HTTP/browser drain gate and stops only this held child. `RuntimeFacade::shutdown()` / `runtime.stop` stop both the main supervisor child and the MCP child, retaining the drain state. Setup failures can leave private partial files inside their UUID instance for diagnosis; no prior installation is replaced. There is no automatic tunnel restart or background health poller; the host schedules health checks.

The foreground profile schema and CLI are from the [official v0.0.16 configuration reference](https://github.com/openai/tunnel-client/blob/v0.0.16/docs/configuration.md). The official [supervision choices](https://github.com/openai/tunnel-client/blob/v0.0.16/README.md#for-codex--copilot) distinguish foreground `run` from managed `runtimes connect`. Upstream readiness predicates come from `launcher/electron/runtime-supervisor.cjs`, especially `readLocalTunnelHealth` / `probeTunnelMcpTransport`, not from a descriptor's existence.

## Native Codex routing and dynamic catalog

```rust
use cw_runtime::{IntegrationJournal, compile_native_route, dynamic_catalog};
let journal = IntegrationJournal::new(profile.clone());
journal.install_native_route("http://127.0.0.1:YOUR_PORT")?;
// or runtime.dispatch("integration.installNative", json!({"endpoint": endpoint}))?
// Serve dynamic_catalog(native_catalog, &capabilities, full_mode, context_override)?
// as the bridge's current /v1/models output; native_catalog must be real or explicitly a fixture.
journal.restore()?;
```

`compile_native_route(text, endpoint)` produces a valid top-level native route. `install_native_route` compiles the actual owned `codex-home/config.toml` baseline, journals it before mutation and installs it under an advisory lock. Both paths:

- Require a loopback root or `/v1` endpoint and install `openai_base_url = "http://127.0.0.1:<port>/v1"`.
- Install upstream's exact route comment and `experimental_realtime_webrtc_call_base_url = "https://chatgpt.com/backend-api/codex"`, so Voice bypasses the local Responses bridge.
- Keep the built-in provider: absent `model_provider` or explicit `"openai"` is accepted and preserved byte-for-byte. Custom provider selection is refused. No custom provider table or `model_provider` is synthesized. Existing route URLs or conflicting Voice URLs are refused, without a replacement override.
- Preserve native model choices, feature flags, comments, BOM and EOL style; do not synthesize `model_catalog_json`, change model context settings, or force compatibility-v1 features.

The application-owned version-1 journal stores full previous/installed bytes and the exact two managed assignment lines. Unknown journal versions are refused. With unchanged installed bytes, restoration reproduces the original bytes or original absence exactly. If unrelated keys changed, restoration first validates TOML, native provider, both managed key lines and the exact route marker, then restores only those two keys/comment. User changes to either managed assignment, provider selection, or marker cause refusal before any write. Newer unrelated edits survive. Existing single-line upstream Voice assignments are retained as exact prior lines. Multiline managed assignments fail closed; this is not an upstream v2–v10 migration implementation.

`dynamic_catalog(native, capabilities, full_mode, context_override)` delegates to the source-bound Rust contracts catalog policy (`cw_contracts::models::augment_native_model_catalog`) with native subagent semantics. It retains native rows and metadata, replaces stale `chatgpt-web/` rows, and generates current routed rows/capabilities/context limits rather than pinning a static model file. JSON action `catalog.augment` takes `{native, capabilities, fullMode, contextWindow?}`. This crate does not fetch account catalogs or start a model HTTP listener itself; the bridge must supply real native metadata and serve the resulting JSON. The route compiler alone does not implement native request forwarding.

## Other APIs and boundaries

`Profile::default_isolated()` uses `~/.codex-web-gpui`; public accessors are `root()`, `codex_home()`, `browser()`, `user_data()`. Generic serde `read_json`/`write_json` reject absolute child paths, parent traversal and existing symlink components. Private atomic files use exclusive temporary creation, sync, rename and parent sync. Canonicalize a trusted OS temporary parent before constructing a profile through aliases such as `/var`. This is not an adversarial same-user filesystem sandbox: directory traversal still has TOCTOU races and no directory-handle/openat/hardlink defense.

The JSON facade additionally supports `diagnostics`, `config.read`, `config.write`, `integration.install` (legacy whole-document helper), `integration.restore`, `runtime.start`, `runtime.poll`, `runtime.stop`, `mcp.status`, `mcp.stop`. Prefer `integration.installNative` for the UI's isolated route action. Config read/write is real owned persistence with basic loopback/port/distinct-name validation, not exhaustive upstream schema migration. Diagnostics report the owned main PID, separate active HTTP/browser counters, and bounded redacted lifecycle records. Records are private and best-effort persisted; child output is discarded.

`Invocation::new(absolute_native_binary, args)` executes directly. `Invocation::packaged(resources,args)` resolves `resources/bin/codex-web-gpui-runtime`; packaging must supply that binary, so tests never treat a nonexistent bundle path as runtime success. The main `Supervisor` uses only its held Child handle, reaps exits when polled and restarts failed exits at most five times in a rolling 60 s. No arbitrary persisted PID can be signaled. Public low-level `configure_tunnel` / `start_tunnel` remain descriptor/direct-spawn primitives; they do not provision or establish readiness and must not back the UI's setup success path.

Remaining parity gaps: upstream dual recovery journals/migrations, interrupt-hook installation, compatibility-v1 feature edits, authenticated control HTTP, background supervisor/tunnel health scheduling, tunnel auto-restart, descendant process-tree termination, graceful MCP drain handshake, exhaustive config migrations/connector rules, record reload and stdout/stderr capture, background service/update installation, Windows/Linux native login backends, signed-app login acceptance, Gatekeeper/notarization packaging behavior, real-account model metadata, real MCP tool transport and public connector/account E2E. No Codex minimum version is invented: version checks cover the owned journal, named-profile schema and pinned tunnel executable; upstream's full Codex capability compatibility is not certified here.

## Launch-on-login and window state (new GPUI identity only)

Read before implementation: `launcher/electron/autostart.cjs`, `window-state.cjs`, `tests/window-state.test.cjs`, autostart/readback fixtures in `tests/runtime-supervisor.test.cjs`, and hidden-window startup in `main.cjs`. Native macOS signatures/status values and login-event constants were checked against local SDK `SMAppService.h` and `AERegistry.h`.

Public API:

```rust
pub const APPLICATION_ID: &str = "dev.alkindi.codex-web-gpui";
// macOS only; this constructor is inert and does not query/register any login item:
let backend = NativeLoginItem::for_profile(profile.clone());
let controller = LaunchOnLogin::new(profile.clone(), backend)?;
// Explicit application actions only; NEVER invoked during this verification:
// let status: LoginItemStatus = controller.status()?;
// let enabled = controller.set_enabled(true)?;
// let disabled = controller.remove()?;

let store = WindowStateStore::new(profile.clone());
let restored: WindowState = store.load(&work_areas)?;
let mut tracker = WindowStateTracker::new(profile.clone());
tracker.schedule(&snapshot, std::time::Instant::now())?;
tracker.flush_due(std::time::Instant::now())?; // caller schedules after 400 ms quiet
tracker.close(&snapshot)?;                   // immediate final write
```

`LoginItemBackend` exposes `identity() -> &str`, `status() -> Result<LoginItemStatus>` and `set_enabled(bool) -> Result<()>`; it allows isolated fixture tests without calling the OS. `LaunchOnLogin<B>` rejects any identity other than the new GPUI ID. `status`, `set_enabled`, and `remove` return the explicit `{supported, enabled, requires_approval}` OS observation. Enabling requires supported=true, enabled=true and no pending approval. Disabling/removal requires enabled=false and no remaining pending approval. A refused or pending request is an error, never a copied requested preference. Removal is idempotent for an already-disabled item. Confirmed status is atomically persisted in the selected profile's `runtime/login-item.json`; it is not treated as current OS truth on the next launch. Constructors never auto-apply saved preferences or enable login startup.

macOS `NativeLoginItem` uses `SMAppService.mainAppService` with `registerAndReturnError:` / `unregisterAndReturnError:` and subsequent `status` readback. Before every read/change it requires the default owned `~/.codex-web-gpui` profile and verifies the actual `NSBundle.mainBundle.bundleIdentifier` equals `dev.alkindi.codex-web-gpui`. It never takes an arbitrary label, scans login items, invokes launchctl, creates a LaunchAgent, or unregisters the original `dev.codexwebgpt.launcher` item. Other native platforms are not implemented. Native registration requires a compatible signed, packaged macOS app; unbundled/foreign identity or custom-profile attempts fail closed. This avoids registering the test runner or changing which profile will open at the next login.

`SMAppService` main-app registration does not accept upstream's arbitrary `--hidden` argument or `openAsHidden` preference. For equivalent UI behavior, the caller can read `NativeLoginItem::was_launched_at_login() -> Result<bool>` while the open-application AppleEvent is current, combine it with explicit `--hidden`, and apply `should_start_hidden(hidden_requested, onboarding_complete)`. As upstream does, incomplete onboarding remains visible. The native login-event probe is read-only and identity-checked; it was compiled but not invoked in tests. Actual login launch timing, approval UI, code-signing acceptance and hidden presentation remain signed-app acceptance checks, not fixture-proven claims.

`WindowBounds` holds finite width/height and paired optional x/y; `WindowState` adds maximized/fullscreen; `WorkArea` contains each display's finite rectangle. `normalize_window_state(&Value, &[WorkArea])` follows upstream's defaults 1120×720, minimum 720×600, maximum dimension 16384, strict true booleans, greatest-overlap display selection, visible top-edge clamping and off-screen position removal. Without display information it preserves finite saved coordinates. Invalid displays are ignored. A workspace smaller than the minimum retains the minimum size and reachable top-left corner, as upstream does.

`WindowStateStore::load` reads only `user-data/window-state.json` under the selected Profile. Missing/corrupt content falls back to normalized defaults; path-isolation violations remain errors. Display normalization returns an in-memory view and does not overwrite the persisted bounds, so disconnecting/reconnecting a display does not discard the last valid geometry. `save` rejects nonfinite/out-of-range dimensions or unmatched x/y before writing a private atomic file. Invalid transient measurements preserve existing state.

`WindowSnapshot` supplies current bounds, normal bounds, maximized/fullscreen and destroyed flags. `capture_window_state` chooses normal bounds during maximize/fullscreen and skips destroyed windows. `WindowStateTracker` exposes the 400 ms debounce with injected monotonic `Instant`, plus immediate close persistence; callers provide GPUI events and timers. No desktop event hooks or background timers are installed by the runtime. Primary owns window/tray/caller wiring.

Tests are entirely temporary: fake login backend defaults disabled, own-item roundtrip/removal preserves an unrelated original-item fixture, denied/approval readback cannot report enabled, corrupt window fields fall back, debounce/close use normal bounds, destroyed/invalid observations do not overwrite, and display changes retain on-disk geometry with 0600 permissions. The real OS login backend was only compiled; no native status/register/unregister/login-event call ran, so actual app launch-on-login stayed disabled.

## Primary app delegation note

Read `crates/app/src/platform.rs` **without editing it**. Its existing adapter writes `dev.alkindi.codex-web-gpui.login.plist`, preserves literal executable / `--profile` / profile root / `--hidden` arguments, enables the launchd label and removes only an exact matching definition. It is a different backend from runtime's identity-checked `SMAppService.mainAppService`. Do not enable both: that could produce two login launches.

The runtime exports these macOS convenience functions so primary can delegate default-profile UI actions without duplicating the tested controller/readback policy:

```rust
pub fn set_autostart(enabled: bool, profile: &Profile) -> anyhow::Result<LoginItemStatus>;
pub fn autostart_status(profile: &Profile) -> anyhow::Result<LoginItemStatus>;

// A caller that keeps the old Result<()> signature can delegate explicitly:
// cw_runtime::set_autostart(enabled, profile).map(|_| ())
// Prefer retaining LoginItemStatus to display actual enabled / requires_approval.
```

These wrappers are explicit operations, not constructors or startup side effects. Their native backend only accepts the new bundle identity and default isolated profile. They are **not** an argv-preserving drop-in replacement for a custom-profile LaunchAgent. If primary chooses ServiceManagement for default-profile launches, it can remove its duplicate adapter source during its own integration, but must separately consider any previously registered owned LaunchAgent. Runtime does not automatically migrate/delete that existing label, and this verification neither queries nor changes it. Arbitrary-profile launch-on-login support remains outside this backend; such calls fail rather than silently reopening the default profile.

Source inspection confirms `crates/app/src/main.rs` currently chooses a centered 1180×820 initial window. Runtime normalization intentionally retains upstream's 1120×720 fallback. Primary must choose the product's first-launch default when wiring persistence; runtime has not silently changed the live GPUI default. The reported successful `dist/codex-web-gpui.app` launch is primary-provided evidence. Runtime window validation remains **source and temporary-fixture evidence only**: no Accessibility measurements, screenshots, physical display-change checks or native window restoration acceptance were obtained here.

## Verification and execution safety

Tests use temp directories, actual test-owned native processes and ephemeral loopback HTTP listeners. The tunnel fixture is Rust source compiled with `rustc`, archived in memory, checksum-verified, installed into the isolated UUID instance and actually run; no missing packaged-runtime placeholder or shell script is used. Its server exposes healthz, readyz and MCP logs. Tests cover ready setup, explicit profile/argv/key references, health loss, fatal MCP logs, checksum tampering, version refusal, endpoint rejection, drain refusal, exact route restoration with unrelated edits, changed managed key refusal, unknown journal version and dynamic native catalog preservation. The original integration tests cover traversal/symlinks, exact BOM/CRLF rollback, non-owned process survival, restart exhaustion, private permissions and pipe deadlock avoidance.

Only crate-scoped tests and formatting checks were run. No real tunnel credentials were supplied, no official tunnel binary was started, no public tunnel/VPS was contacted, and no production Codex config, existing profile, original application, route, tunnel settings or `~/Library/LaunchAgents` was modified. Official GitHub source pages were read to verify v0.0.16 CLI/schema. Workspace Cargo resolves shared lock metadata as a build side effect; root manifests/vendor/other crate source are not manually edited. No vault writes fall within this delegated scope.

Focused verification history is recorded below.

Final focused run: `CARGO_TARGET_DIR=crates/runtime/target cargo test -p cw-runtime -j 2 --locked` — **16 passed, 0 failed** (7 unit tests including native MCP fixture/local HTTP tests and AST-based multiline decoy safety; 9 integration tests; 0 doc tests). Build 2.06 s, unit tests 1.83 s, integration tests 0.21 s. Formatting check: `rustfmt --check --edition 2024 crates/runtime/src/lib.rs crates/runtime/src/mcp.rs crates/runtime/src/route.rs crates/runtime/tests/runtime.rs`. Earlier compilation exposed borrowed temporary URL strings; fixed with owned URL bindings before the successful runs. The contracts dependency is now used for dynamic catalog generation and has been restored only in the runtime manifest; `zip` provides bounded in-process extraction without external tools.


Lifecycle follow-up verification: `CARGO_TARGET_DIR=crates/runtime/target cargo test -p cw-runtime -j 2 --locked` — **17 passed, 0 failed** (8 unit, 9 integration, 0 doc). Build 3.51 s; unit suite 2.73 s; integration suite 0.23 s. Added an end-to-end native fixture lifecycle test covering Tokio facade move/return, fresh status, same-facade restart, busy restart refusal, competing-facade lock refusal, orderly drop/reopen reconnect, stale-owner refusal, changed key/profile/executable/companion refusal and saved-instance traversal refusal. Only `crates/runtime/src/mcp.rs` and this document changed in this follow-up. `rustfmt --check --edition 2024 crates/runtime/src/lib.rs crates/runtime/src/mcp.rs crates/runtime/src/route.rs crates/runtime/tests/runtime.rs` passed. Production configuration, original app/profile, external tunnel and VPS were not touched; no real credentials were supplied.


Desktop-policy follow-up: `CARGO_TARGET_DIR=crates/runtime/target cargo test -p cw-runtime -j 1 --locked` — **22 passed, 0 failed** (13 unit, 9 integration, 0 doc); build 6.64 s, unit suite 3.16 s, integration suite 0.25 s. Five desktop-policy tests were added. Reused the existing target with one compiler job; no second build target or bundle was created. Scope: `crates/runtime/src/desktop.rs`, `src/desktop/macos.rs`, runtime exports/target dependency and this document. No actual login item/LaunchAgent was registered or removed. This adds focused desktop APIs; it does not establish original journal compatibility or full runtime/application parity.

Primary-delegation follow-up verification: existing target, `cargo test -p cw-runtime -j 1 --locked` with `CARGO_TARGET_DIR=crates/runtime/target` — **22 passed, 0 failed**; build 5.81 s, unit suite 3.41 s, integration suite 0.24 s. Runtime formatting check passed. `crates/app/src/platform.rs` was read-only; no caller wiring, actual autostart operation, app launch, Accessibility request or screenshot capture was performed.
