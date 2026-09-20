# Hermes backend implementation plan

> Execute inline, preserving the remote-browser branch. Test new behavior before implementation.

**Goal:** Add a separately configured Hermes model backend that reuses the browser transport without taking over Hermes tool execution.
**Spec:** ../specs/2026-09-20-hermes-backend.md
**Stack:** Existing Bun 1.4.0 / TypeScript / MCP SDK and a dependency-free Python Hermes provider plugin.

## Global constraints
No live config/service mutations, no new JS runtime dependencies, loopback-only bearer-authenticated inference, separate broker/tunnel/connector, no new Codex route or model passthrough. Hermes canonical transcript and native executor remain authoritative. Sol automatic routes only initially.

## Review focus
- Tool-result retries must preserve the original call ID and browser turn, while a new session must not inherit it.
- Missing session provenance, user XML and unsupported fields must not widen tool authority.
- HTTP disconnect/errors must not become successful answers or leaked browser turns.
- Profiles and auxiliary requests must not share process-global session state.
- Plugin discovery and the real Hermes transport must carry provenance without mutating the original messages.

## Tasks
1. Request adapter and private provenance.
   - Files: src/hermes/request.ts; tests/hermes-request.test.ts; types.ts; environment.ts; conversation-key.ts; index.ts.
   - Interface: parseHermesRequest(value, namespace) -> CodexParsedRequest; private _hermes context consumed only by shared identity/environment functions.
   - RED: identity same across tools, different across users/profiles; exact multimodal/tool payload; malformed inputs fail.
   - GREEN: convert Chat history through the existing Responses parser; attach trusted context; keep Codex branches unchanged.
2. Chat Completions transport and HTTP boundary.
   - Files: src/hermes/completion.ts, server.ts; tests/hermes-server.test.ts.
   - Interface: startHermesServer(config, optional adapterFactory) -> Bun server; chat JSON/SSE reducer consumes existing AdapterEvent.
   - RED: real HTTP auth/catalog; text/tools/errors/stream events; real broker cycle with deterministic browser worker.
   - GREEN: reuse existing adapter, broker, abort lifecycle, usage and fixed model route definitions.
3. Dedicated setup/MCP/connector routing.
   - Files: src/hermes/config.ts, cli.ts; existing cli.ts, tunnel.ts, mcp-main.ts, mcp-server.ts, browser-worker.ts.
   - RED: isolated homes, no Codex config writes, connector search query and generic-only tool surface.
   - GREEN: explicit setup/serve/tunnel/status commands; runtimeCommand embeds isolated home; owner-only key/config.
4. Official provider plugin and installation.
   - Files: integrations/hermes/model-provider/{__init__.py,plugin.yaml}; installer and tests under integrations/hermes.
   - RED: real Hermes registry and Chat transport imports under temp HERMES_HOME A/B; unchanged messages; auxiliary isolation.
   - GREEN: documented ProviderProfile hooks; profile-scoped session metadata; no core edits.
5. Verification and documentation.
   - Run root typecheck/tests, launcher tests, runtime build, plugin tests; inspect diff for secrets and unintended changes.
   - Record research coverage and actual limitations; provide exact setup commands, rollback boundaries and live acceptance checklist.
   - Commit and push feature/hermes-backend, not the existing remote-browser branch.

## Execution ledger
- Baseline: fork at 432440e, clean; typecheck passed; remote/setup baseline tests 6/6.
- Research: official reference pinned at 8b42b6e020c3f5b50c555625c81eccf1bb97de56; entire tracked tree inventoried, relevant provider/agent/tool docs and source reviewed. No claim of line-by-line review of all 14,366 files.
- Ruling: use Chat Completions plugin hook rather than bare Responses URL: official generic Responses route supplies neither Hermes session metadata nor Codex environment authority. The supported Chat hook supplies stable session identity without patching Hermes.
- Tasks 1–4: implemented. New Hermes request/server/config/MCP modules, official out-of-tree provider plugin and non-overwriting installer are present. Existing Codex paths remain the default.
- Real provider test caught an incorrect `context_length` capability key. RED reproduced `None != 90000`; GREEN uses official `context_window`, verified again through `agent.models_dev.get_model_capabilities`.
- Real AIAgent test initially stalled because its deterministic browser fixture assumed all native tools were directly advertised. Ruling: preserve native deferred discovery; the fixture now invokes `tool_describe` / `tool_call` when Hermes advertises that gateway. No production executor bypass was added.
- Native AIAgent end-to-end contract: real HTTP/adapter/broker/MCP plus native write_file, read_file, terminal, todo_list and memory passed. Only the browser/model is simulated. A configured `approvals.deny` probe returned BLOCKED and did not create its target file.
- Profile isolation: official provider and transport imports passed A→B→A tests using environment switching and Hermes context-local home overrides. Messages remain unchanged; stateless auxiliaries carry no synthetic session ID.
- Verification: 18 new Hermes tests; 152 runtime regression passes + 1 platform skip; 101 additional CLI/server/native-route/MCP regression passes; all 310 launcher tests; 2 official provider tests; 1 real AIAgent test; TypeScript check; runtime bundle build.
- Full root suite attempts exceeded execution budgets, including slow tokenizer/usage cases. They are incomplete, not an all-pass claim. Completed regression evidence and live acceptance exclusions are recorded in docs/hermes-research.md.
- Task 5: setup, rollback, safety limits and research scope documented in docs/hermes-backend.md and docs/hermes-research.md. Review performed inline; no independent reviewer/subagent result is claimed.
- Deployment boundary: no live Hermes/ChatGPT credentials or user services changed; account-bound browser/connector tests and simultaneous physical Mac/Hermes operation remain live acceptance checks.
