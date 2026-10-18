# Rust Responses and turn-bound MCP bridge

Reference: upstream **v6.1.10**, SHA `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5` (verified vendor HEAD). The bridge executes no local commands/tools and uses no Bun/Node wrapper. Browser actions belong to the isolated GPUI browser adapter; native tool execution and approvals belong to the **outer Codex harness**.

## GPUI integration API

```rust,ignore
let (bridge, mut requests) = BridgeServer::bind(
    "127.0.0.1:0".parse()?,
    BridgeConfig { mode: persisted_mode, ..Default::default() },
).await?;
while let Some(request) = requests.recv().await {
    match request {
        BrowserRequest::Turn(turn) => {
            // Start exactly this browser owner with turn.task.prompt/images.
            // Browser progress -> turn.events, observe turn.cancellation.
            // Manual: present/copy prompt; after user confirms it was sent:
            // bridge.confirm_manual(&turn.request_id, &turn.owner)?;
        }
        BrowserRequest::ToolCall(call) => {
            bridge.forward_native_tool(call).await?;
            // Do not execute this tool in GPUI or return "not implemented".
            // Outer Codex receives the real function/custom call via Responses,
            // performs approval/execution, and submits its correlated output.
        }
        BrowserRequest::Compaction(request) => {
            // Dedicated checkpoint lifecycle below; never an ordinary task send.
        }
    }
}
bridge.close().await?;
```

`forward_native_tool` is the GPUI integration name (alias of `handoff_tool`). `handoff_tools(Vec<NativeToolCall>)` emits a single parallel batch. `handoff_tool` handles a single invocation. These methods retain each call's oneshot until actual outer-harness output arrives; they never synthesize a result. The legacy public `NativeToolCall.result` remains for a separately supplied genuine harness/test fixture, but the GPUI integration uses `handoff_tool`.

`BridgeServer::start` aliases `bind`. `BridgeHandle.local_addr` / `base_url` expose the selected listener, including ephemeral port zero. `set_mode(ExecutionMode) -> Result<(), String>` changes no listener/port and rejects active browser turns, pending tools, native requests or active checkpoints. Initial mode comes from `BridgeConfig.mode`. `execution_mode`, `authorize`, `confirm_manual`, `cancel`, `drain`, `is_draining`, `counters`, and asynchronous idempotent `close` are public. Handle clones share one listener; the owner calls `close` explicitly.

`BrowserTurn` contains request ID, owner, random turn token, compiled task, context, requested model/effective effort, mode, event sender, cancellation receiver and launcher confirmation receiver. `CompiledTask.raw` retains unknown request fields; server-resolved replay input and effective route effort can be expanded/defaulted just as upstream parsing resolves them. The bridge has no filesystem/shell capability.

## Implemented outer tool continuation

1. MCP `codex_tool_call` validates the current bearer turn capability, owner claim, deadline, and exact source registry wire name. Wire names use upstream `namespace__name`. Structured arguments and freeform input remain distinct.
2. GPUI forwards `ToolCall` to `handoff_tool(s)`. The current Responses observer emits function/custom/tool_search call items, argument/input done events, `response.completed` with **`end_turn:false`**, and `[DONE]`. Namespaced functions carry separate native `namespace` and `name`; custom calls carry raw `input`. The call ID is exactly the pending MCP invocation's ID.
3. This ends only the HTTP round. The browser, capability, native-tool oneshot, event receiver and owner slot remain alive. No replacement browser prompt is submitted.
4. The next Responses request correlates by stored `previous_response_id`, or by native thread/turn and outstanding call IDs when Codex supplies full history. It validates model/owner, output type, duplicate IDs and full parallel-batch coverage before releasing any result. Invalid/partial/cross-owner outputs leave pending calls untouched. An already active observer rejects a competing continuation.
5. String or multimodal `function_call_output` / `custom_tool_call_output` converts to the source broker CallToolResult: text, inline data-URL images or remote resource links, parsed object/array `structuredContent`. An actual serialized MCP CallToolResult envelope is returned intact, preserving content, structuredContent, _meta, isError and unknown fields; explicit metadata attached to native output items is preserved too. Actual output alone resolves MCP. The same browser can then continue using the same token and submit its final answer through the new Responses observer.
6. An outstanding round retried without results replays the same call items/IDs in stable order. Completion returns **`end_turn:true`** and retires the owner/capability. Cancellation/expiry in an observer gap still releases pending MCP requests and the browser slot.

A process-local replay cache retains complete input plus authoritative completed outputs, bounded by one hour, 1,000 entries and a 64 MiB high-water eviction policy. Unknown `previous_response_id` follows source expansion semantics (leave the supplied body untouched and try full-context/live-call correlation); it is not rejected wholesale. Known retired pending-tool rounds are rejected, never restarted as a browser prompt. Persistence across bridge process restart is not implemented.

## Manual and compaction

Manual mode advertises native source names `codex_turn_start` and `codex_turn_complete`, in addition to inventory/call. Both launcher Sent confirmation and connector start are required for tool access. Start is idempotent while live; completion requires nonempty real final answer, no pending tool invocations and no conflict with an already accepted answer. Completion itself produces the Responses assistant answer without inventing browser DOM success. Automatic mode does not advertise those controls.

Compaction is classified from source protocol, not rejected wholesale:

- Native text compaction requires `client_metadata.x-codex-turn-metadata.request_kind=compaction`, `compaction.implementation=responses`, `strategy=memento`. It uses the exact upstream checkpoint instruction and returns assistant message summary output.
- Remote v2 `compaction_trigger` runs a summary browser task, then emits **exactly one** `compaction` output item with `ocx1:` + base64(UTF-8 actual summary). It does not emit an assistant message in that output. Empty summary is failure.
- Replayed compaction/compaction_summary/context_compaction decode transparent ocx1 checkpoints to source summary framing. Opaque OpenAI encryption remains explicitly unreadable; a payload-free context_compaction marker is dropped. It does not start a new compaction task.
- Work tools are unavailable during compaction. Ordinary live-tool continuation and a separate compaction request are distinguished; an overlapping owner is not silently resumed into ordinary work tools.
- Browser compact-v1 `/responses/compact` and `/v1/responses/compact` now return replacement history: original real user items/metadata within the source 80,000 UTF-16-code-unit text budget plus newest ten structured non-sentinel images, followed by one user summary with source single-newline prefix. AGENTS refreshes, internal/goal fragments and prior readable summaries are excluded. There is no v2/encrypted item in compact-v1 output. Native passthrough still forwards native compact-v1 to the official backend. The canonical `x-codex-turn-metadata` HTTP header overrides compact-v1 body metadata as in source. Luna's non-Bigger-Context separate-compaction refusal is preserved.

## Model catalog and native passthrough

Public injection methods:

```rust,ignore
bridge.set_account_capabilities(observed_caps)?; // cw_contracts::models::ChatGptWebAccountCapabilities
bridge.set_native_model_catalog(original_native_catalog_json)?;
bridge.set_model_catalog_options(ModelCatalogOptions {
    full_mode: true, compatibility_v1: false, context_override: None,
})?;
// Only when explicitly configured by the app:
bridge.set_native_passthrough_base("https://chatgpt.com/backend-api/codex")?;
```

`set_account_capabilities` accepts the actual typed `cw_contracts::models::ChatGptWebAccountCapabilities`. `catalog_status() -> CatalogStatus` returns a serde-serializable snapshot: capabilities_observed, native_catalog_available, native_catalog_fetched, augmentation_succeeded, verified, last_http_status, last_error, successful_requests, native_models, routed_models and last_success_at. **verified is false for DOM capability observation alone and for injected fixtures**, even after successful augmentation. Only a successful authenticated official fetch followed by successful augmentation sets verified. Changed capabilities, mode, catalog or catalog options invalidate augmentation/verification; unchanged repeated capabilities do not. HTTP catalog errors revoke verified and record their status/error. `BridgeCounters.active_native_requests` is also serde-serializable.

Capabilities use source camelCase names (`solAvailable`, `proAvailable`, optional `extraHighAvailable`, `experimentalBiggerContext`, `zeroRiskProEnabled`). The bridge supplies the actual selected Manual/Automatic mode. Absent account observation or absent official catalog is an explicit HTTP error; neither is fabricated. GET `/models` and `/v1/models` preserve native catalog unknown fields and original rows, augmenting with pinned source routes through `cw-contracts::models::augment_native_model_catalog`. This dependency was added after inspecting the implemented contracts model API; no contract source was edited. Compatibility/context overrides are explicit options, not guessed settings. Routed requests are checked against observed availability and source effort policy.

Passthrough is disabled by default. Its base setter accepts **only** `https://chatgpt.com/backend-api/codex` (optional trailing slash), no arbitrary origin, credentials or query URL. Once enabled, non-`chatgpt-web/` Responses requests and native `/responses/compact` forward to that official backend using **only incoming harness Bearer authorization** and end-to-end request headers. No keychain, `~/.codex`, environment credential, existing browser cookie/profile or VPS is read. Redirects are returned rather than forwarding credentials to another destination. Native model GET can obtain an original catalog using the incoming harness auth when one was not injected. Recognized first-party User-Agent versions add native models `client_version`; conditional catalog ETags are removed. Bridge-owned compaction/reasoning artifacts are scrubbed at the native boundary. Native response bytes stream through with cancellation/backpressure; `[DONE]` ends meaningful SSE and no terminal success is fabricated for truncated upstream data. Native requests participate in mode-change admission and close cancellation.

Responses defaults to JSON unless `stream:true` is explicit, matching the source schema/parser. GET `/v1/responses` returns source-compatible 426 (WebSocket transport unavailable). POST aliases `/responses` and `/v1/responses`, GET `/health` and `/healthz`, and POST `/mcp` serve loopback only. Native passthrough has not been exercised against a real account in this task.

## Prompt/compiler source boundary

`src/fixtures/prompt-contract.json` extracts actual inline shared/tool/compaction contract strings and checkpoint instructions from pinned upstream, with SHA/hash provenance; a test checks every static literal against vendor source. Automatic token transport uses the original `codex_transport_resume` sentence, not the previous ad hoc appended instruction. Manual uses `codex_zero_risk_request_json`. Inline context is fenced source **version 3**, with system instructions separate and literal developer/user/assistant/agent_message/tool_result semantics; historical tool calls retain IDs/namespaces and assistant phases. Complete raw wire request/registry definitions remain available separately. Delimiter escaping preserves decoded data. Retired historical turn/request/binding handles are scrubbed from serialized string values, leaving structural keys/native call IDs intact; current native tokens use the agreed longer random format. Images remain separate attachments; source latest-ten selection, omission markers and one-pixel sentinel removal are implemented. Original image detail is preserved for native browser adapters.

Additional tools carried in input are included in the registry. Hosted tools are not falsely listed as local native tools; source-incompatible nondefault namespaced custom tools are not exposed. Opaque encrypted-only agent messages fail before browser submission.

## Remaining named gaps

- No upstream Unix-socket broker compatibility, binding/activity RPCs or durable journal. Native in-process channels implement the actual Responses tool roundtrip. Full revision-based completion fences and process-crash replay remain incomplete. Actual ordinary final text that wins the compaction race is retained for exact canonical-tool-result continuation replay; this is a narrow producer-authenticated tombstone, not complete general final/phase/reasoning replay.
- Manual connector start/complete is functional; upstream surface-nonce verification, setup-only TTL extension, exact retry receipts and completion/activity revision reconciliation are not complete. Current configurable TTL bounds the whole turn.
- Compact-v1 and retained mid-tool checkpoints work in fixtures, including native-text and remote-v2 response formatting and exact-request deduplication. Starting a checkpoint while an ordinary Responses observer is still active is explicitly refused rather than stealing that owner. Full retained-conversation lineage/recovery for already retired browser pages, synthetic delegation/source-revision recovery, ocxr1 reasoning/signature replay, Bigger Context staging, Luna rolling checkpoints and skill-file transport remain incomplete. Fresh no-source `/responses` compaction still uses the existing explicit compaction BrowserTurn; compact-v1 uses the new CompactionRequest path. UTF-16 budget slices at a surrogate boundary are rendered safely with a replacement codepoint rather than emitting an invalid Unicode surrogate.
- Prompt inline core contracts/envelopes are ported. No claim of byte-for-byte equality across every dynamic branch: superseded model-switch filtering, all reasoning/provider item mappings, complete token budgets and structured-output validation still require parity work.
- Trusted XML environment reconstruction, sandbox/workspace lineage and local approval policy remain the **outer harness's responsibility**. The bridge executes nothing locally and treats task metadata/token as correlation, not independent account authentication.
- MCP generic inventory/call, Manual controls, direct-native convenience aliases, Manual safeVisibleTools and exact 30-second wait-agent schema/enforcement work. Nested exec-gateway programs/discovery remain unsupported; raw unnamespaced exec fails closed in Automatic mode because its upstream JavaScript transport guard is not ported. Full Zod transport-envelope bounds/defaults, SDK sessions/resumable transport and batch JSON-RPC remain gaps. Scoped cancellation notifications and a bounded observational ledger are implemented below; sessionless cancellation by request ID alone is intentionally not authorized.
- Real browser/account/tool/visual acceptance remains separate. All tests here use loopback fixtures or construct official native requests without sending them; no production app/profile/routes/tunnels/VPS or external API was contacted.

## Verification

Run only this crate using the existing target: `CARGO_BUILD_JOBS=1 cargo test -p cw-bridge -j1 --offline`. The test suite covers role/image/raw-field preservation, source prompt literals, terminal SSE and monotonic sequence numbers, manual gating, capacity/isolation/expiry, MCP multimodal results, pending cancellation/drain, real outer-harness function/custom continuation, parallel replay/partial/duplicate/owner rejection, source-native and remote-v2 compaction, original catalog augmentation, idle-only mode changes, strict native origin and incoming-auth enforcement. Final verification is refreshed below for this continuation. No workspace/app rebuild or new target directory was performed by this worker.

## Retained checkpoint API and physical ownership

The public additions are `BrowserRequest::Compaction(CompactionRequest)`, `BridgeHandle::confirm_compaction_source_settled(request_id, owner) -> Result<(), String>`, `BridgeCounters.active_compactions`, and the pure `build_compact_v1_output(input: &Value, summary: &str) -> Vec<Value>`. Existing forward_native_tool, typed account capabilities, catalog_status and mode APIs remain unchanged.

CompactionRequest carries `request_id`, `source_request_id: Option<String>`, native `owner`, `model`, `effort`, compaction-only `task`, exact-purpose `prompt`, `control_token`, `handoff_id`, `mode`, `reuse_manual_response`, `source_finished`, `accepted`, `cancellation`, and `completion: oneshot::Sender<Result<(), String>>`. Its task has no work registry. Never send `task.raw.tools` or an ordinary source token with the checkpoint.

1. `source_request_id: Some(id)` binds the original source page/conversation and native owner. Do not resend the source task. `source_finished` becomes true after bridge consumes the source semantic terminal event following its canonical tool results. It is **not** physical/helper retirement.
2. Automatic: wait for source_finished, wait for actual source helper cleanup, then call confirm_compaction_source_settled. The checkpoint can allocate the next helper epoch on that same retained page only after the old helper physically settles. Submit exactly `request.prompt` as one checkpoint message. A source-less request can allocate a fresh **compaction-only** page/epoch.
3. The `control_token` grants only `codex_tool_call` with `wire_name=codex.control.compaction_handoff`, exact handoff_id, and nonempty summary. Wrong owner claims/handoff, freeform input, expired/consumed capabilities, work calls and inventory are rejected. The source work token is revoked after semantic source completion, but its browser owner/watch is retained until cleanup acknowledgment.
4. `accepted == true` is an actual MCP receipt. Stop/settle the checkpoint helper without waiting for assistant DOM text or Copy. Send completion Ok only after physical cleanup. Then the source retirement watch is released and compact HTTP can return. Cancellation/error sends logical failure promptly but retains admission/ownership until completion is acknowledged or the owned bridge closes. Completion Err must likewise mean cleanup has finished. Dropping completion is not proof of release.
5. `reuse_manual_response == true` sends **no second message** and starts **no new helper/DOM turn**. The last canonical native tool result keeps its actual content/metadata and receives the source Zero Risk compaction instruction. The original request_id completes through codex_turn_complete. Wait for accepted, then acknowledge cleanup without DOM observation. Fresh Manual checkpoints instead use the new request's summary-only request_id/start/complete controls, with explicit readiness confirmation after user Sent.

All outstanding delivered tool outputs are validated before changing a live owner or releasing a single result. Automatic canonical results are unchanged; only queued/future unexecuted tool calls receive the exact source CODEX_ACTIVE_COMPACTION_REQUEST interruption result. Manual adds the source checkpoint instruction to the final actual result while preserving existing metadata. The completed summary is producer-canonicalized with CODEX_LATEST_USER_PROMPT_JSON and conflicts are rejected. Exact requests share a 30-minute, bounded in-memory checkpoint operation, including disconnected HTTP observers; successful retry does not resend the checkpoint. Failure permits a new attempt only after physical ownership has been released.

The source modules inspected for this port are responses/compaction.ts, server.ts compactRequest, native-compaction-control.ts, compaction-transaction.ts, compaction-handoff.ts settleActiveCompactionSource/settleActiveZeroRiskCompactionSource/requestRetainedCompactionHandoff, index.ts retained/fallback orchestration, turn-broker.ts requestCompaction/invoke, and compaction-continuation.ts. Source tests read include compaction-v1.test.ts and retained-compaction tests. The native app's actual helper-epoch/page reuse must still be verified independently after wiring.

## MCP source parity and remaining transport boundaries

Source: pinned `adapters/chatgpt-web/mcp-server.ts` and `mcp-observation.ts`. No local tool implementation or JavaScript evaluator is introduced.

| Area | Implemented behavior / remaining gap |
| --- | --- |
| Convenience aliases | `codex_exec` selects exact unnamespaced exec_command, then shell_command; approval fields are forwarded only when the native schema declares each supplied field, otherwise rejected. Shell mapping preserves source command/timeout_ms behavior. `codex_write_stdin`, `codex_apply_patch`, `codex_view_image` resolve only exact advertised native tools. apply_patch preserves native freeform versus structured input. Alias schemas/bounds and annotations follow source; unknown alias fields are stripped from the forwarded payload. Missing direct native tools fail explicitly. |
| Nested execution | Upstream nested gateway programs/discovery and fallback remain unsupported. Raw unnamespaced exec also fails explicitly, since forwarding unguarded JavaScript would bypass upstream recursive-exec/wait protections. No inferred tools, fallback success, eval, command execution or approval decisions occur in this bridge. |
| Wait schema and invocation | Exact names multi_agent_v1__wait_agent, multi_agent_v2__wait_agent, collaboration__wait_agent require numeric timeout_ms=30000. Inventory preserves unknown native schema fields, removes conflicting timeout default, sets type number/const/min/max=30000, deduplicates string required entries and appends source polling description. Validation is repeated at public native handoff. The native harness performs the actual wait; this is the source polling-argument rule, not fabricated wall-clock completion. |
| Manual visibility | safeVisibleTools removes bridge convenience/control names, their entire namespaces, reserved compaction control wire and unnamespaced exec, both at inventory and invocation/handoff. Automatic retains the native registry but rejects unsupported raw exec explicitly. Compaction still exposes no work tools. |
| Notifications | `notifications/cancelled` uses standard params.requestId plus an explicit capability scope: params.turn_token in Automatic or params.request_id in Manual. No request ID alone may cancel an unrelated owner. Unknown/finished/ambiguous IDs do not cancel a call; notifications receive 202 and no JSON-RPC response. Other notification methods are acknowledged without invented effects. This stateless HTTP scope differs from SDK connection-scoped cancellation; SDK sessions/resume and standard session-bound ID-only cancellation remain unsupported. |
| Cancellation lifecycle | A unique scoped request cancellation wakes the NativeToolCall cancellation watch and releases the MCP waiter with an error. Before handoff, forward_native_tool rejects the cancelled/closed call. After handoff, the already emitted outer invocation cannot be undone: its real correlated continuation is consumed, its abandoned oneshot receipt is discarded, and no new browser prompt is submitted. This is logical MCP cancellation, not a claim that the outer harness stopped an approved tool. The owner remains live until its normal terminal/cleanup path. |
| Observation ledger | `BridgeHandle::mcp_observations() -> Vec<McpObservation>` exposes sequence, event, known convenience-tool classification or unknown, elapsed_ms, call correlation, outcome and is_error. Both observations and tracked IDs are capped at 1024. Duplicate IDs within a capability become uncorrelated tombstones; cancellation of either ambiguous request is refused. No arguments, results, tokens, request IDs or arbitrary tool names are exposed. Events include call_received, reply_prepared, request_dropped and cancellation observations. reply_prepared is handler preparation, **not socket delivery or native execution**; reply_sent/send_failed require transport acknowledgements this HTTP handler does not provide. Full source revision/activity RPC fences remain a separate gap. |

Alias responses retain the actual CallToolResult envelope, including content, structuredContent, isError, _meta and unknown fields. Existing GPUI routing remains `BrowserRequest::ToolCall(call) => bridge.forward_native_tool(call).await`; app code must not execute tools. BrowserRequest and CompactionRequest interfaces did not change for this bounded task.

### Explicit logout API

`BridgeHandle::clear_account_capabilities() -> Result<(), String>` rejects any live turn owner, pending tool call, native request or active checkpoint. When idle it clears observed typed capabilities, the native catalog/status (verified=false), response history, idle checkpoint records and private pending MCP correlation tombstones. Configured official-origin passthrough/options remain configuration. No bearer credentials are cached or borrowed: every future passthrough still requires fresh inbound harness authorization. Catalog verification must again come from an actual successful fetched-and-augmented official catalog; the app must not infer it from DOM observations.

## Final bridge-only verification for compact continuation

Verified 2026-10-18 using the existing target directory: `CARGO_BUILD_JOBS=1 cargo test -p cw-bridge -j1 --offline` passed **31 tests** (1 unit + 30 integration; zero failures). The new compaction suite contains eight tests: replacement-history and one-shot control; metadata/image/UTF-16 retention; canonical mid-tool source reuse; cleanup/admission after cancellation; Manual original-request completion; invalid-input atomicity and publishable-final replay; native-text/remote-v2 streaming/retry; wrong handoff/owner/conflicting summary and control expiry. No external calls or authenticated browser/account evidence were used. `rustfmt --edition 2024 --check` passed for bridge Rust sources/tests. No new target directory, workspace/app build, source/vendor/shared-crate edit or production operation was performed by this worker.

Changed scope for this continuation: crates/bridge/Cargo.toml, src/compaction.rs (new), src/protocol.rs, src/server.rs, src/session.rs, src/catalog.rs, tests/compaction.rs (new), and this ledger. GPUI must wire the new Compaction variant and actual helper cleanup before real retained-page acceptance can pass.

## Bounded MCP/logout verification, 2026-10-18

`CARGO_BUILD_JOBS=1 cargo test -p cw-bridge -j1 --offline` passed **39 tests** (5 unit, 34 integration; zero failures), using the existing target and no new dependencies. New checks cover exact alias approval/shell mapping and validation, Manual namespace visibility, wait schema preservation/30-second rejection, bounded content-free observations, HTTP alias freeform payload/result metadata, wrong-scope cancellation, duplicate-ID ambiguity, cancellation before/after actual outer SSE handoff, real continuation consumption without another browser prompt, and logout rejection/invalidation. The prior compaction/continuation/owner/SSE suites also passed. Rustfmt check passed for bridge sources/tests. No external requests, app/workspace builds, account operations or local model-tool execution were performed.

Files changed in this bounded pass: src/mcp_policy.rs (new), src/mcp_ledger.rs (new), src/server.rs, src/session.rs, src/catalog.rs, src/compaction.rs (idle-cache clearing only), tests/mcp_policy.rs (new), tests/protocol.rs (source alias list count), and this parity document. All paths are under crates/bridge except this document.
