# Native browser adapter parity

Reference: `vendor/upstream`, v6.1.10, `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`.
Status: native automatic submission/observation implementation with mock transport tests; **authenticated ChatGPT and full native input acceptance remain unverified**; 19 isolated fixture DOM cases passed in WebKit and owned CEF. No installed Codex app, production CDP/profile, cookies, tunnel, or VPS was used.

## Integration contract

`cw-browser` has no browser launcher, listener discovery, Node/Bun/Electron subprocess, login, cookie import, or implicit navigation. Primary creates CEF with an isolated cache and ephemeral loopback CDP port. Primary must prove the exact randomly named `about:blank#cw-<uuid>` target from its own endpoint, then supply that page's `webSocketDebuggerUrl`. `connect` accepts only explicit `ws://127.0.0.1:<port>/devtools/page/<id>` (or IPv6 loopback), not browser endpoints or hostnames. URL validation cannot establish ownership by itself: the caller's target provenance is required.

```rust
pub struct Ownership { pub task_id: String, pub epoch: u64 }
pub struct Attachment { pub path: std::path::PathBuf }
pub enum InteractionMode { Automatic, Manual }
pub struct TurnInput {
    pub connector_name: Option<String>, // exact configured connector, None for smoke
    pub full_prompt: String, // serde fullPrompt, alias full_prompt
    pub model: String,      // exact routed slug, no inference/fallback
    pub effort: String,
    pub attachments: Vec<Attachment>,
    pub interaction_mode: InteractionMode, // serde interactionMode
    pub ownership: Ownership,
    pub turn_token: Option<String>, // literal caller token; no fixed length/prefix
}
```

- `BrowserEngine::connect(ws_url: &str) -> Result<Self>` is async. It only connects; it does not evaluate or navigate the initial blank page. Retain it as `Arc<BrowserEngine>` (Send + Sync); do not reconnect for every turn.
- `inspect_auth(&self) -> Result<AuthInspection>` is async; fields are `readable`, `composer`, `server_authenticated`. The last requires the same-origin server session endpoint to return a user without a session error or expired/invalid expiry. Raw session credentials are never exported. Readable DOM/composer alone never proves authentication. `inspect_auth()` remains an automatic-only shorthand. Mode-aware primary UI should use `inspect_auth_for_mode(mode, cancellation)`; Manual returns `Error::ManualDomForbidden` before a request. Do not call the automatic shorthand as a side effect of manual submission.
- `navigate(&self, url: &str) -> Result<()>` is async, explicit, and limited to ChatGPT HTTPS URLs. It addresses the connected target only. Primary invokes it only after proving ownership. It now enables lifecycle events and waits for the exact returned main-frame/loader load event; unrelated loads cannot satisfy it. Same-document navigation needs no new loader. `navigate_with_cancellation(url, cancellation)` exposes cancellation. Neither method reads DOM, including Manual. `wait_ready(timeout, cancellation)` is an explicit automatic-only DOM probe for a complete document and one composer/form; automatic submit also calls this probe before capability/model work, so the first submit waits for hydration.
- `submit(&self, TurnInput, tokio::sync::mpsc::UnboundedSender<Progress>, Cancellation) -> Result<TurnResult>` is async. Model resolution uses `cw_contracts::models::require_chat_gpt_web_model_route`; all current and legacy automatic route identities in the pinned contract catalog are handled, including Luna and Think. Route-owned legacy adapter effort and optional/unpinned family are preserved. Current Luna low/medium selects ordinary/Think respectively; legacy Think keeps its Luna backend and adapter medium even when Codex reasoning is low. Unknown routes, unsupported current effort, missing exact family, locked ticks, or unavailable Think fail before Send. Legacy routes intentionally preserve upstream unpinned family semantics; they are not an inferred fallback for an explicit version.
- `inspect_account(mode, cancellation) -> Result<AccountInspection>` is async and returns a SHA-256 account digest (`userId + NUL + accountId`), plan type, personal/workspace identity, attention flag, server-authenticated evidence and `cw_contracts::limits::Plan`. It reads the server identity twice and fails on changed identity/plan, missing proof, or delinquent supported subscription. Personal `pro` maps to Pro200, personal `prolite` to Pro100; other plans/structures are explicitly Unsupported. Raw user/account IDs are not retained in public results, and no session token/cookie leaves the page. It does not open a picker or require a composer.
- `inspect_capabilities(mode, cancellation) -> Result<CapabilityInspection>` is async. `inspect_capabilities_with_options(mode, CapabilityOptions { selector_timeout, stable_absence }, cancellation)` allows caller-bounded inspection; defaults match upstream's full 30-second selector budget and 3-second stable absence. A transient missing selector never proves Luna. Two selector-presence observations precede a single consistent slider/tick/lock snapshot. Account identity is server-verified before and after the probe. Results contain `account`, contracts `capabilities`, observed `families`, `available_efforts`, `think_available`, and `routes: Vec<AvailableRoute { route, efforts }>`. Contracts routes are filtered by observed family/effort/Think evidence; explicit model identity is reverified at submission. These are observed picker availability, not a guarantee that every server model turn will succeed. Missing/malformed picker evidence is an error, not an invented free/Plus/Pro capability. Manual account/capability inspection returns `ManualDomForbidden` without CDP/DOM activity.
- `zoom(factor) -> Result<NativeZoomRequest { factor, cef_zoom_level }>` is synchronous and pure, matching the bundled gpui-cef host's `ln(factor)/ln(1.2)` conversion. Primary applies the request using the CEF host; the adapter reports no fake applied zoom. Manual uses this same DOM-free path. Nonfinite/nonpositive factors are rejected.
- `Cancellation::default()`, `cancel(&self)`, `is_cancelled()`, `cancelled().await`. Primary may map `watch<Option<String>>` to this token, including an already-present cancellation reason. The adapter reports `Error::Cancelled`; primary retains its original reason.
- `Progress::Accepted { ownership, user_turn_id }`; `Assistant { ownership, turn_id, text, html }`; `Commentary { ownership, turn_id, text }`. Progress text/HTML are complete current preview projections, not deltas. Do not stream these snapshots into append-only Responses text: raw innerText and formatted Markdown have different prefixes. Deliver the final `Completed.text` atomically.
- `TurnResult::Completed { ownership, user_turn_id, assistant_turn_id, text, html }` (`text` is final coherent Markdown; `html` retains raw assistant evidence) or `ManualHandoff { input }`. Manual submit returns the original input without any DOM evaluation, model selection, navigation, typing, sending, or synthetic completion. Primary owns Copy/Sent UI, confirmation deadlines, and any later manual response transport.
- `stop(&self, &Ownership)` is explicit and rejects the wrong task/epoch. Cancel and await submit first, then request Stop if desired. Cancellation invalidates async helper continuations. Before Send it awaits owned draft rollback; after Send it awaits the document-bound Stop/final boundary and idle cleanup. It does not implicitly close the target. `close(&self)` explicitly sends Page.close to this target only. Both serialize behind active submit.

Map primary `owner.thread_id` to task_id and allocate an epoch per native turn_id. Retain that mapping and the same engine for retries. Supply `task.prompt` as full_prompt and the actual compiled `turn_token` only if its full literal value occurs exactly once in that prompt. There is no 32-character limitation: the bridge currently supplies `turn_` plus 64 hex characters, and other literal nonempty markers are accepted. Browser matching uses indexOf rather than constructing a regex, counts duplicates, and retains upstream ASCII marker-boundary protection so a prefix of a longer marker cannot acknowledge acceptance. For primary images `{ref,image_url,detail}`, materialize validated bytes into application-owned files and pass their paths; preserve ref/detail/token accounting in the bridge. This crate does not fetch arbitrary image URLs, assign image detail, or own temporary-file lifetime.

A Send attempt is reserved **before dispatch**, including uncertain acceptance. Same task/epoch retries fail `DuplicateSend`; failure is never permission to retry Send. Preflight refuses existing user text/attachments or wrong pills without editing. From the first owned edit, preparation failures await fresh-token rollback; uncertain or abandoned cleanup quarantines the engine. Cancellation/timeout/transport failure may follow acceptance. Persist receipts in primary before retiring the engine; the current latch is in memory, not durable across a new connection/process. A target is bound to one task and monotonically increasing epochs once automatic submit begins.

## Implemented behavior and source map

| Native file | Upstream source / behavior |
| --- | --- |
| `crates/browser/src/cdp.rs` | Native WebSocket request IDs, concurrent reply multiplexing, bounded handshake/request waits, protocol/disconnect errors, cancellation and dropped-request cleanup, broadcast target events. Replaces Playwright/helper IPC runtime. |
| `crates/browser/assets/page.js` | `src/chatgpt-session.ts` composer, controls, slider, send/stop, and exact family/announcement rules from `model-selection.ts`. Selection checks family radio, slider range/ticks/locks, each keyboard step, and family/name/Pro announcement before typing. |
| `crates/browser/assets/user-message.js` | `browser-user-message.ts`: self-contained upstream helper with TypeScript annotations removed; rejects unknown markup and preserves plain/rich text semantics. |
| `crates/browser/assets/classifier.js` | `browser-worker.ts` CHATGPT_COMMENTARY_CLASSIFIER block, with TypeScript annotations removed. Uses activity/status/COT ownership and first-status ordering to separate commentary from answers. |
| `crates/browser/src/lib.rs` | Browser worker logical baseline: `data-turn-id-container`, stable `data-turn-id`, and both group roles for every `data-turn-key`; never display-index/test-ID binding. Exact new-user text or single broker token acceptance, unique new response binding, incremental projections, 2-second text+HTML completion stability with following assistant controls and no Stop. Document replacement fails safely. |
| `crates/browser/src/inspection.rs` / `selection.rs` | `chatgpt-session.ts`, `limits.ts`, adapter `model.ts` and contracts model routes: stable capability absence/presence, locked slot evidence, server identity/plan digest, catalog resolution and distinct Luna/Think behavior. |
| `crates/browser/src/markdown.rs` / `smoke.rs` | Native structured conversion and complete-delivery draft semantics from `markdown.ts`/`chatGptMarkdownContent`, plus the exact browser-worker smoke marker and Sol/high vs Luna/low choice. |
| `crates/browser/src/upload.rs` | `attachment-upload.ts`: owned-frame request/file-name matching, same-origin creation/processing endpoints, HTTP failure detection and processing-completed receipts, plus native DOM.setFileInputFiles and composer card confirmation. |

The browser scripts are versioned browser assets, not a JavaScript application runtime. The full upstream MIT license is retained in `crates/browser/assets/LICENSE.upstream`. The minimal native driver replaces `browser-helper-main.ts` orchestration and the Electron `runBrowserPageScript` execution wrapper with CDP evaluations and request deadlines.

## Markdown and delivery API

```rust
pub fn render_assistant_markdown(html: &str, text_fallback: &str) -> cw_browser::Result<String>;
pub struct MarkdownError {
    pub reason: String,
    pub html: String,
    pub text_fallback: String,
}
pub struct AtomicMarkdownBuffer { /* private draft state */ }
// Default::default(), then:
// observe(&mut self, html: &str, text_fallback: &str) -> Result<()>
// finish(&mut self) -> Result<String>
```

`render_assistant_markdown` uses a native HTML5 parser and native Rust conversion rules derived from upstream `markdown.ts` and `chatGptMarkdownContent`. It preserves ATX headings, emphasis, compact/nested/ordered/task lists, blockquotes, code language/indentation and safe fence lengths, GFM header tables/alignment, real link destinations/titles, inline file-path links, Obsidian prose links, standalone proposed_plan markers, unique TeX annotations, unique writing-card bodies, and CodeMirror logical lines. Observed UI buttons, hidden helpers, chart widgets, image/SVG layers and file preview chrome are excluded as in upstream; actual download links and visible plain file labels are preserved.

HTML is authoritative. Only absent/blank HTML permits explicit plain-text fallback. Unknown rich elements, ambiguous math/card/code sources, headerless/spanned/inconsistent tables, and malformed supported structure return `Error::Markdown(MarkdownError)` with both raw inputs intact. Display/log text includes only the reason, not response content. The native conversion deliberately fails on some ambiguous structures upstream keeps as raw/generic content; it does not claim complete Turndown/GFM equivalence.

Upstream `ChatGptMarkdownBuffer` ordinary streaming commits ordered semantic blocks after 750 ms of stability, then reconciles stable source ranges/node identities and text/link/order changes. Mutable file-reference boundaries hold back the tail. Its complete-delivery mode instead maintains an undelivered draft until finish. The native final-output path uses complete delivery: observe replaces a draft, never appends a snapshot; finish delivers once, and a repeated finish returns empty. Final settling compares rendered Markdown for two seconds, excluding toolbar/KaTeX visual hydration noise; real answer/link edits restart settling.

Without the full upstream block ledger, native code does not attempt to reconstruct a virtualized answer from guessed text. If final source extents lose an observed prefix/tail, or the final output is a strict prefix/suffix of a previously longer coherent snapshot, delivery fails with raw final evidence rather than silently truncating. This guard is intentionally conservative and can reject a legitimate shortened revision. Full source-range/node rebind reconciliation and incremental block streaming remain parity gaps. Preview events remain replacement snapshots only.

Source-native smoke helpers are exported: `SMOKE_PROMPT = "Reply with exactly: CODEX WEB GPT READY"`, `SMOKE_EXPECTED = "CODEX WEB GPT READY"`, `smoke_turn_input(&CapabilityInspection, Ownership) -> Result<TurnInput>`, and `validate_smoke_response(&str) -> Result<()>`. The constructor follows source generic Sol/high (`chatgpt-web/high`) or Luna/low selection through contracts and observed availability, without pinning an invented GPT-6 Instant smoke route. Construction does not navigate/probe/send. Primary must explicitly invoke submit on its idle owned target after the user chooses Run test; no account smoke was executed during development.

## Dedicated CHECKPOINT settlement

This browser-only summary-control path is independent of connector/work-tool selection. It sends the caller's exact compiled CHECKPOINT prompt once on the current owned target, after primary has physically awaited the source helper. It never constructs/exposes a work-tool registry, invokes a work tool or executes a native command. Existing `submit` keeps its signature and final Markdown behavior.

```rust
pub struct CheckpointRequest {
    pub source_owner: Ownership,
    pub turn: TurnInput,
    pub control_token: String,
}
pub async fn settle_checkpoint(
    &self,
    request: CheckpointRequest,
    accepted: tokio::sync::watch::Receiver<bool>,
    cancellation: Cancellation,
) -> Result<CheckpointSettlement>;
pub struct CheckpointSettlement {
    pub ownership: Ownership,
    pub receipt_accepted: bool,
    pub outcome: CheckpointOutcome,
    pub cleanup: CheckpointCleanup,
}
// CheckpointOutcome: Accepted | Cancelled | ReceiptClosed | ReceiptTimedOut | Failed(String)
// CheckpointCleanup: Confirmed | Unconfirmed { reason: String }
// settlement.retirement_confirmed() requires receipt_accepted && cleanup == Confirmed.
```

Primary supplies a dedicated, initially false, monotonic watch for the **actual accepted summary receipt** for this exact request. DOM text, completion controls, helper success and early final answers never substitute for that receipt. The exact source owner must match the engine; the checkpoint uses the same task at exactly source epoch + 1. A summary-only control token must occur once in the prompt; `turn_token` must be None and attachments empty. The prompt is not rewritten. Manual returns `ManualDomForbidden` before acquiring the gate or touching the page; primary handles manual compaction without a browser helper.

The existing engine mutex, preparation/verification path and attempted-send latch are reused directly; normal `submit` is never invoked from settlement. Acceptance, cancellation, closed receipt, send failure and receipt-wait timeout all lead to awaited cleanup after Send was attempted. Receipt waiting is bounded to 180 seconds and never waits for final DOM Markdown. Cleanup uses a fresh token, an awaited document-bound Stop/evaluation barrier and two subsequent idle observations. If generation already finished, an unambiguous post-baseline user/control-token and completed assistant identity may prove that boundary without Markdown conversion. A missing Stop alone is not cleanup proof. Each CDP evaluation remains bounded; the idle poll window is five seconds.

`Confirmed` means the Rust helper has left receipt waiting and its owned-page Stop/final boundary plus idle checks completed; it is not a claim that an unobserved remote server operation was cancelled. Live CEF/account acceptance remains separate. `Unconfirmed` never acknowledges retirement, even when the actual summary receipt was accepted. The engine is quarantined before Send until cleanup is confirmed, including if the caller drops the future. Further normal/checkpoint sends return `CleanupUnconfirmed`; primary must retain the unsettled state and handle explicit owned-target teardown/reconciliation. Callers must signal cancellation and await the settlement future, not abort it. No background submit helper is spawned by this API.

## Verification performed

- `cargo test -p cw-browser`: 44 Rust tests (20 unit + 7 inspection integration + 17 Markdown integration). Local ephemeral mock WebSocket only; out-of-order replies, timeout, cancellation, protocol error, continued connection usability, loopback page-target restriction, logical virtualized-turn baselines/ambiguity, exact route rejection, manual zero DOM requests, automatic accepted/output flow and duplicate accepted Send, arbitrary/long literal broker-token uniqueness/Arc compatibility, upload receipt parsing, every current/legacy automatic contract identity, Luna/Think backend binding, locked/family route filtering, server digest/plan/account-change proof, stable Luna absence, manual zero-request capability/account/zoom paths, owned lifecycle navigation, readiness timeout/cancellation, source-native smoke policy, structured Markdown and raw-evidence failures, atomic draft replacement, duplicate finish suppression, missing-source/prefix rejection, and six CHECKPOINT tests covering acceptance before final DOM, cancellation, early DOM final without a receipt, owner/manual rejection, closed receipts, and cleanup-failure quarantine.
- `cargo clippy -p cw-browser --all-targets --no-deps -- -D warnings`: browser-only strict lint. The initial all-dependency strict invocation was blocked by existing `collapsible_if` lints in `crates/contracts/src/models.rs` (outside assigned scope); that crate was left unchanged.
- `swift crates/browser/tests/page-assets.swift`: native macOS JavaScriptCore parses all three browser assets and exercises the extracted commentary ordering/COT plain/unsupported user-text helpers, and long/regex-special/duplicate/mismatched/prefix token receipts. Swift is a development check only; production remains Rust + CEF.
- Reviewed source/test contracts and captured composer, power complete/streaming/activity, activity summaries, DIL and CodeMirror fixtures. Native Markdown tests execute the captured CodeMirror, DIL smoke and completed power HTML against expected output, including exact upstream CodeMirror/DIL expectations. This does **not** execute the complete DOM automation inside CEF or prove account interaction.

## Remaining named parity gaps

1. **CEF/account acceptance:** isolated-profile sign-in, actual CEF focus/key input/menu hydration, real successful account turn, upload, cancellation and sign-out have not been run. Model selection can fail safely on slow hydration or changed DOM. No real-account E2E claim.
2. **Picker activation recovery:** aria-controls, aria-labelledby and unique structural-menu resolution are implemented. Full upstream ghost-menu cleanup, pointerdown activation, close/reopen commit verification and longer slider-hydration recovery are not yet ported. Availability/identity checks fail rather than falling back.
3. **Remaining response fidelity/streaming:** native structured HTML-to-Markdown and final coherent delivery are implemented and fixture-tested. Complete Turndown/GFM edge-case equivalence, every citation/rich renderer variant, full source-range/node block reconciliation, mutable-reference incremental streaming, activity status summaries, revision caching and DOM-health/rebind recovery remain incomplete. Unknown rich content is an error with raw evidence. Conservative final-prefix/source guards may reject valid shortened edits; no response after a tool batch is proven by this adapter alone.
4. **Upload stream lifecycle:** current receipt parsing waits for loadingFinished + Network.getResponseBody. Upstream Network.streamResourceContent/dataReceived incremental UTF-8 processing and completion-before-cancel recovery are absent. Such uploads fail closed/time out rather than being deemed accepted from a card. Composer cards currently require exact aria-label evidence; aria-labelledby-only cards are not yet recognized.
5. **Connector/tool protocol:** connector personalization/selection, native MCP broker calls, external tool progress, confirmation handling, staged multipart/Bigger Context and full source compaction orchestration are not implemented here. Dedicated owned-page CHECKPOINT receipt settlement is implemented; primary still owns the compaction protocol, receipt authority, source-helper sequencing and UI. Luna/Think and current/legacy automatic catalog selection are now implemented through cw-contracts. These require coordination with bridge/runtime; unsupported model routes are rejected.
6. **Retained conversation orchestration:** native primary must assign/retain target leases, provide fresh/saved URL choices and task/epoch mappings, enforce five-tab capacity and recycle targets. This adapter neither discovers tabs nor switches task ownership. Full reload or changed assistant identity currently fails; upstream bounded React rebind/recovery is not ported.
7. **Durable receipts and observation resume:** attempted-send latches live for this engine instance. Reconnect/process restart requires primary durable reconciliation; there is no resume-only observation API. Never reconstruct an engine and automatically resend an uncertain/accepted turn.
8. **Manual native UX and image mapping:** primary owns Copy/Sent workflow, manual deadlines/readback, image URL-to-local-file preparation and cleanup. Manual handoff is intentionally not automatic execution.
9. **Session/terminal errors:** auth session evidence is distinct from readability, but expired-session/rate-limit dialogs, localized rejection classes, connector/session refresh and upstream diagnostic redaction/trace export are not fully ported.
10. **Inspection/plan runtime proof:** capability and plan rules are implemented and mock-tested; live server response schema, picker variants, real subscription/usage classification and native host zoom/readiness still need isolated CEF/account acceptance. Missing supported server identity fails safely, including when the page itself is readable.
11. **Cancellation delivery:** signal cancellation/timeouts discard pending CDP replies and invalidate async helper continuations; already-dispatched synchronous clicks/input cannot be undone. Dropping an entire submit future without cancelling its token is not the supported cancellation protocol. Once accepted there is no absolute generation timeout, matching upstream; caller must cancel abandoned turns.

Vault bookkeeping is left to primary because this bounded assignment permits writes only in `crates/browser/**` and this parity ledger.


## Fresh review verification, 2026-10-19

Shared preparation and final Send integrity are implemented in `src/preparation.rs`; connector preflight/uniqueness in `src/connector.rs`. Exact selected path is retained, so a same-name pill with a changed path also fails the final gate. Controlled cases include attachment failure after file input mutation, cancelled insert/verification/upload, early checkpoint receipt, abandoned future, unknown cleanup, preservation of initial content, disabled/late/transient duplicate names and post-preparation prompt/pill changes. Workspace verification passed 184 Rust tests; 19 DOM cases passed separately in real WebKit and owned CEF with a fixture-injected origin. Evidence and limits: [handoff](handoff.md).
