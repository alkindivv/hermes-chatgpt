# Hermes backend research and verification record

Date: 2026-09-20. Implementation branch: `feature/hermes-backend`.

## Source baseline and coverage

Official repository: `https://github.com/NousResearch/hermes-agent`.

The inspected reference checkout is pinned to `8b42b6e020c3f5b50c555625c81eccf1bb97de56` (package version 0.21.3). The browser bridge is based on this fork's `432440e`, including the remote Electron implementation. A later upstream Hermes main commit was visible during the work; compatibility claims in this document concern the pinned checkout, not an unreviewed moving main branch.

The entire tracked tree was inventoried: **14,366 files**, including **468 files under `website/docs`**. That count is an inventory, not evidence of having read every line. Review focused on the integration's execution path and associated official instructions/documentation. This is **not** a line-by-line audit of all repository files or all official documentation.

Tracked-tree distribution at the reference commit:

| Area | Files | Role in the investigation |
| --- | ---: | --- |
| `agent/` | 302 | Agent loop, transport and session/context boundaries |
| `providers/` | 3 | Provider profile and registry/discovery seam |
| `plugins/` | 389 | Existing provider/plugin extension patterns |
| `tools/` | 320 | Native dispatch, deferred tools, approvals and terminal/file boundaries |
| `hermes_cli/` | 526 | Provider selection and profile/configuration surfaces |
| `gateway/` | 170 | Same agent core through messaging surfaces; no platform adapter changes |
| `cron/` | 32 | Scheduler remains external to model transport; no scheduler rewrite |
| `skills/` | 331 | Capabilities stay with Hermes; no copied skill runtime |
| `tests/` | 4,759 | Official behavior contracts and architecture reference |
| `apps/` | 2,921 | Client applications remain unchanged |
| `website/` | 850 | Includes documentation and site assets, not all prose |

Other directories (optional skills/MCPs, UI, evaluations, catalogues and contributors) were structurally inventoried. They were not all semantically audited and are not represented as fully tested by this change.

## Load-bearing references

The review used official checkout documentation and concrete source contracts, including:

- Root `AGENTS.md`, `agent/AGENTS.md`, `tools/AGENTS.md`: narrow core, plugin-first extension, profile isolation, cache and native tool invariants.
- `website/docs/developer-guide/model-provider-plugin.md`: registration/discovery, profile capabilities, `build_extra_body`, and the distinction between a model provider and a general tool plugin.
- `website/docs/developer-guide/agent-loop.md`: same `AIAgent` loop for model requests, native tool execution and transcript persistence.
- `website/docs/developer-guide/programmatic-integration.md`: external agent-control protocols are different from adding a model backend.
- `providers/base.py`, `providers/__init__.py`, `agent/transports/chat_completions.py`: tested directly through their real imports, not a copied interface.
- `run_agent.py`, `agent/AGENTS.md`, native tool discovery/dispatch paths: agent-level tools cannot be replaced safely with an independent registry runner.
- `hermes_constants.py`: request-time profile context overrides must take precedence over process environment.
- `agent/models_dev.py`: plugin capability metadata uses `context_window`; merely declaring an unknown `context_length` key does not provide the intended contract.
- `tools/file_tools.py`, `tools/terminal_tool.py`, `tools/todo_tool.py`, `tools/memory_tool.py`, `tools/tool_search.py`: actual native tool schemas, including the current `todo_list` name and deferred `tool_describe` / `tool_call` interface.
- `tools/approval*.py` and the documented `approvals.deny` contract: denial must remain authoritative before command execution.

Related official online docs were used for cross-checking: model-provider plugins, agent loop, tools runtime, provider runtime, and context compression/caching. Where a cached online page differed from the pinned source, the executable source contract was the testing authority.

## Architectural decisions

### 1. Keep execution inside Hermes

A standalone Python executor would miss agent-owned memory/todo state, deferred tools, session policies and approval hooks. The bridge therefore returns OpenAI-style function calls to the actual Hermes loop and waits for its results. No Hermes core files are modified.

Evidence: the native-agent test instantiates the real `run_agent.AIAgent` with this provider and executes native file, terminal, todo and memory operations. No registry replacement or mocked Hermes client/dispatcher is used.

### 2. Use the official Chat Completions provider hook

The earlier idea of setting a generic Responses base URL is insufficient evidence of native compatibility: the browser bridge needs stable session provenance and must not treat user-authored Codex XML as permission metadata. The plugin's `build_extra_body(session_id=...)` provides the required seam without changing Hermes.

The authenticated HTTP endpoint converts messages/tools using the existing bridge parser and attaches trusted internal Hermes identity only after authentication. Request-provided `_hermes` fields and fake environment blocks cannot grant Codex authority.

### 3. Separate capabilities, share only the browser host

The Hermes API token, broker socket, tunnel profile and `Hermes ChatGPT` connector are separate from the Mac's `Codex Native2`. Source changes do not migrate or overwrite the Mac setup. An operator must select a distinct Hermes tunnel ID; a bridge on the VPS cannot inspect every tunnel configured on the Mac.

The same physical Electron host can serve both consumers, subject to its original tab/account limits. Simultaneous live Mac/Hermes use is an acceptance test, not something proved by the offline fixture.

### 4. Preserve canonical history rather than pretending retention is universally safe

Hermes owns compression and history edits. A completed Hermes user turn does not leave a hidden browser conversation available for incremental reuse by the next user turn. Tool-result requests belonging to one ongoing turn do continue the same browser response, preserving exact tool IDs and results.

### 5. Preserve deferred discovery

Real Hermes may hide a tool such as `todo_list` behind its native `tool_describe` / `tool_call` interface. The bridge must not silently add the hidden tool. The native-agent fixture initially exposed this mismatch in its own test assumptions; the fixture was changed to use Hermes's advertised gateway, and the production executor boundary remained unchanged.

### 6. Bound transport state and report uncertainty

Long MCP tool calls are polled with a stable operation ID rather than resubmitted. The cache is bounded and tied to capability retirement. This is not durable exactly-once execution after a process restart; failed side effects require reconciliation. Errors and incomplete streams must not become successful final answers.

## Verification actually completed

| Check | Result |
| --- | --- |
| New Hermes TypeScript tests (5 files) | 18 passed, 0 failed |
| Runtime boundary regression set (11 files) | 152 passed, 1 platform skip, 0 failed |
| CLI/server/catalog/native-route/MCP regression set (9 files) | 101 passed, 0 failed |
| Entire launcher Node test suite | 310 passed, 0 failed |
| Root TypeScript typecheck | Passed |
| Official Hermes provider discovery/transport tests | 2 passed |
| Real Hermes AIAgent with production bridge/broker | 1 passed |
| Runtime bundle build (`dist/hermes-runtime`) | Passed |

The Python tests use an isolated virtual environment containing the pinned official Hermes package and its dependencies. The provider test covers A→B→A environment and context-local profile switching, untouched source messages, session metadata, model capability lookup and refusal to overwrite an installed plugin.

The native-agent test writes/reads a disposable file, runs `printf`, updates native todo and memory, and attempts a harmless `touch` explicitly blocked by `approvals.deny`. It verifies that the denied file is not created and that the denial result reaches the model side. The full sequence remains one simulated browser response.

**The browser/model is simulated in these tests.** They do not use ChatGPT cookies or a live subscription and are not proof of current ChatGPT DOM/connector/account behavior.

Attempts to run the full root test suite exceeded tool execution budgets. A bounded diagnostic run reached slow tokenization/usage tests before timing out; another broad browser/harness group also did not finish within its budget. Those runs are **incomplete**, not reported as all-pass or as isolated product regressions. The completed sets above are the verified regression evidence.

## Remaining live acceptance

The account-bound checks in `hermes-backend.md` remain necessary: actual ChatGPT sign-in, model selection and connector operation; a native Hermes turn through that real browser; concurrent Mac/Hermes use; reconnect and cancellation; and broader configured capabilities such as skills, browser tools, delegation and cron. The generic native execution path supports advertised tools, but those individual product workflows have not all been exercised end to end here.

No benchmark or claim of full behavioral parity, battery savings, unrestricted model availability, or complete repository audit is made by this record.
