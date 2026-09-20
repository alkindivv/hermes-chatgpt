# Hermes Agent backend (experimental)

This fork can use its authenticated ChatGPT Web browser as a **model backend** for NousResearch Hermes Agent. Hermes keeps its own agent loop, tool dispatch, approvals, memory, skills, terminal backend and canonical transcript. It does not become a Codex subprocess.

The integration was tested against Hermes **0.21.3**, source commit `8b42b6e020c3f5b50c555625c81eccf1bb97de56`, and fork base `432440e`. This is a pinned compatibility baseline, not a claim to support every future Hermes release. See [research and verification scope](hermes-research.md).

## Architecture and boundaries

```text
Hermes Agent
  | official model-provider plugin: hermes-chatgpt
  | authenticated Chat Completions requests
  v
Hermes bridge (127.0.0.1:17842)
  | existing browser adapter / CDP
  v
Electron + authenticated ChatGPT Web
  | separate Hermes ChatGPT connector / MCP tunnel
  v
Hermes turn broker -> function calls returned to Hermes
                       |
                       v
                 native Hermes tool loop
                 approvals, tools, results
```

Mac Codex can continue using the same physical Electron host through the existing remote-browser link. Its `Codex Native2` connector, tunnel, broker, local files and daemon remain separate. The Electron host's existing account/tab limits are shared, not multiplied by adding another consumer.

This implementation deliberately adds an official **Chat Completions provider plugin**, not a bare `codex_responses` endpoint override. The plugin supplies profile/session provenance through Hermes's supported `build_extra_body` hook. Native Hermes tools, including deferred `tool_describe` / `tool_call` tools, continue through Hermes's normal loop. There is no second Python tool runner in this bridge.

The MCP compatibility names `codex_tool_inventory` and `codex_tool_call` are retained internally, but the dedicated Hermes MCP server exposes only the authenticated Hermes request's tool registry. It does not expose Codex shell/patch/filesystem convenience tools.

## What is implemented and tested

- Authenticated `/v1/models` and `/v1/chat/completions`, JSON and SSE streaming.
- Public automatic browser model slugs; no native Codex OAuth passthrough.
- Text, image message parts, function schemas, exact tool-call IDs and results.
- JSON-object / JSON-schema request conversion through the existing adapter.
- Profile- and session-scoped logical turn identity. Tool-result rounds continue the same browser response.
- No hidden browser-conversation reuse between completed Hermes user turns; Hermes remains authoritative after compression, edits or resumed history.
- MCP polling for long native tools, using one operation ID rather than blindly resubmitting side effects.
- Dedicated configuration, API key, broker, tunnel profile and connector.
- Official Hermes plugin discovery, native transport, model capability lookup and context-local profile isolation.
- A real Hermes `AIAgent` test executing file write/read, terminal, todo and memory, including deferred-tool dispatch and a denied terminal command.

**The native-agent test simulates only the ChatGPT browser/model response.** It uses the production HTTP endpoint, adapter, MCP server and broker plus real Hermes tools. It does not prove that an account's current ChatGPT UI, login, connector permissions and tunnel are working. Those require the live acceptance checks below.

No existing Hermes installation, profile, default provider, gateway service or Mac configuration was changed as part of building this branch.

## Requirements

Use Bun **1.4.0** for this fork. Hermes requires the supported Python version/dependencies of the pinned official checkout; the provider plugin itself adds no third-party Python dependencies.

The reference deployment is a trusted Unix VPS. Run the **bridge under the same OS account that owns the Electron browser descriptor**. For example, if Electron runs as `ubuntu`, run the Hermes bridge as `ubuntu`, not root. The descriptor must remain owner-only. Do not loosen its permissions or run Electron as root to avoid this check.

Hermes itself may run under a different trusted account on the same VPS and connect to loopback HTTP using the local API key. Provision that key to its own private secret store without making the Electron profile or tunnel credentials world-readable.

The account needs an authenticated production Electron browser, an available automatic reasoning model, and permission to use the required custom MCP/tunnel integration. This remains unofficial browser automation and does not bypass login, account limits or workspace policy.

Projects are not mirrored to the browser host, but prompts, selected file contents and tool results are still transmitted to that host and ChatGPT. Treat the browser profile, API token, tunnel credentials and resulting account data as sensitive. A shared API token represents one trusted operator, not a multi-tenant security boundary.

## 1. Build this branch

Use a permanent source directory owned by the intended runtime account. Do not install the runtime from `/tmp`.

```bash
git clone --branch feature/hermes-backend --single-branch \
  https://github.com/alkindivv/hermes-chatgpt.git hermes-chatgpt
cd hermes-chatgpt
bun install --frozen-lockfile
bun run scripts/build-runtime-bundle.ts dist/hermes-runtime
```

The entry point below is **this fork's** built runtime, not an upstream `codex-chatgpt-web` command elsewhere on PATH:

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web --help
```

Keep the source/runtime directory in place after configuration: the MCP tunnel launches the recorded runtime command from there.

## 2. Configure a separate Hermes bridge

Keep the existing production Electron host running. The default bridge home is `~/.hermes-chatgpt`, separate from both `~/.hermes` and `~/.codex-chatgpt-web`.

Create a **dedicated Hermes tunnel ID** and obtain the required runtime key. Do not reuse the tunnel connected to the Mac's `Codex Native2` connector. The setup cannot inspect a tunnel configuration on another machine; choosing a distinct tunnel ID is an operator responsibility.

Use either a private key file:

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web hermes setup \
  --browser-host-descriptor /home/ubuntu/.codex-chatgpt-web/runtime/launcher-browser.json \
  --tunnel-id YOUR_DEDICATED_HERMES_TUNNEL_ID \
  --runtime-key-file /home/ubuntu/private/hermes-tunnel-runtime.key \
  --acknowledge-unofficial
```

or an existing environment variable without creating an intermediate plaintext file:

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web hermes setup \
  --browser-host-descriptor /home/ubuntu/.codex-chatgpt-web/runtime/launcher-browser.json \
  --tunnel-id YOUR_DEDICATED_HERMES_TUNNEL_ID \
  --runtime-key-env OPENAI_API_TUNNEL \
  --acknowledge-unofficial
```

The two runtime-key options are mutually exclusive. The environment value is immediately copied into the bridge's owner-only managed secret file; the variable name and key value are not persisted in `hermes.json`.

Setup verifies the production browser and account capabilities, then writes:

```text
~/.hermes-chatgpt/
  hermes.json
  secrets/api.key
  runtime/hermes-broker.sock    # created when serving
  tunnel/...                  # Hermes-only credentials/profile
```

It does **not** call the Codex setup/route installer or write Hermes `config.yaml`.

A custom bridge home is supported through the global `--home` option, for example:

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web --home /home/ubuntu/.hermes-chatgpt \
  hermes status
```

Keep the same home for setup, serve and status. Do not point it at a Codex runtime home.

## 3. Start the bridge and create its connector

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web hermes serve
```

This owns the dedicated HTTP server, broker and Hermes tunnel. Leave it running for the initial acceptance test. SIGINT/SIGTERM closes only this bridge's runtime and tunnel.

Using the account's supported custom-connector workflow, create a **new connector named exactly `Hermes ChatGPT`**, connected to the dedicated Hermes tunnel. Leave `Codex Native2` unchanged. Connector availability and approval policy must permit the actions being requested; this integration does not grant that access itself.

Verify from another terminal under the bridge account:

```bash
./dist/hermes-runtime/bin/codex-chatgpt-web hermes status
./dist/hermes-runtime/bin/codex-chatgpt-web hermes tunnel status
```

The inference port defaults to `17842`; `hermes setup --port NUMBER` changes it. The plugin's default URL must then be overridden through Hermes's normal provider/model configuration. Do not expose the raw listener or CDP publicly.

## 4. Install only the provider plugin in the chosen Hermes profile

Run this under the intended Hermes user, replacing the path with the existing profile's actual home:

```bash
python3 integrations/hermes/install.py --hermes-home /path/to/chosen/hermes-profile
```

The installer creates:

```text
/path/to/chosen/hermes-profile/plugins/model-providers/hermes-chatgpt/
  __init__.py
  plugin.yaml
```

It refuses to overwrite an existing plugin and never edits credentials, sessions or `config.yaml`. Stop/restart a Hermes process to discover the newly installed provider; an already-running process may have cached its provider registry.

Supply `HERMES_CHATGPT_API_KEY` through that profile's private secret configuration. This is the **generated local bridge token**, not an OpenAI model API key and not the tunnel runtime key.

For a one-terminal test when bridge and Hermes use the same OS account:

```bash
export HERMES_CHATGPT_API_KEY="$(cat "$HOME/.hermes-chatgpt/secrets/api.key")"
```

Do not paste the token into chat, commit it, print it in logs, or add it to a command-line argument. For a long-running gateway, use the profile's normal secret mechanism rather than relying on an interactive shell export.

Choose the intended profile by Hermes's normal profile mechanism. An explicit per-process home is also useful for a test:

```bash
HERMES_HOME=/path/to/chosen/hermes-profile \
  hermes chat --provider hermes-chatgpt -m chatgpt-web/high \
  -q "Read the project's README and explain what it contains."
```

This per-call provider choice does not require changing the profile's current default provider. Persist a default only after the live test passes and only as a deliberate separate configuration change.

## Models and inference behavior

Read the bridge's authenticated `/v1/models` catalogue or `hermes status` rather than inventing internal backend IDs. Its enabled public slugs are a subset of:

```text
chatgpt-web/light
chatgpt-web/medium
chatgpt-web/high
chatgpt-web/extra-high
chatgpt-web/pro
```

Extra High and Pro are exposed only if the browser account supports them. Raw internal IDs such as `gpt-5.6-sol` are intentionally rejected by the Hermes endpoint.

The model slug selects the browser effort. Ordinary API sampling controls (`temperature`, `top_p`, token limits, etc.) do not control ChatGPT's browser UI here; the plugin omits temperature, and supported browser limits still apply. Only one completion (`n=1`) and automatic/disabled function-tool choice are supported. No forced named tool selection, logprobs, audio, native Codex routes, Luna rolling checkpoints or Zero Risk/manual mode is claimed.

The provider declares conservative context windows using Hermes's real `context_window` capability field. Existing Hermes compression, budget, fallback and tool policies remain in force. The bridge does not add a paid API fallback; any fallback configured by the operator in Hermes is still Hermes's responsibility.

## Failure behavior and operational limits

- Account expiry, missing connectors and unsupported models are errors, not successful answers.
- Streaming errors are emitted as errors; no synthetic successful completion is substituted.
- Every tool call uses the exact advertised schema/name and returns to Hermes for validation and execution. Deferred tools remain behind Hermes's own discovery gateway.
- Long MCP operations return `pending`; poll the same `operation_id` without changing arguments. Deduplication is limited to the lifetime of that MCP process, not durable exactly-once execution across a crash.
- A failed/restarted agent or bridge can make the outcome of a side effect uncertain. Reconcile the transcript and actual environment before retrying; do not assume a timeout means nothing happened.
- The bridge has an internal 15-minute outstanding-tool deadline and a bounded MCP operation cache (256 operations / 32 MiB). Long approvals, very long tools or an exceptionally large single tool cycle may reach these limits; this is not an unbounded transport.
- CLI/gateway/cron/subagent capability depends on the selected Hermes provider, profile settings and tools supplied for that session. This branch does not force all auxiliary or delegated models to use the browser, or enable previously disabled tools.
- Sharing Electron shares its finite tab capacity. A busy Mac plus Hermes delegation may exhaust it; no extra quota is created.
- Do not embed the Codex and Hermes server entry points in one process. Run separate processes/homes; their process-local lifecycle stores are not a multi-tenant server API.

## Live acceptance checklist

Before using this for important work, verify against the real account:

1. Authenticated model listing and a plain response from Hermes through the Electron host.
2. Read a disposable project file, edit it through Hermes, run a harmless command, inspect the resulting diff.
3. A denied command stays denied. Test a configured approval without changing the policy to bypass it.
4. Memory/todo/skills, the configured browser tool and a single delegated task, then a completed/resumed conversation.
5. A separate Mac Codex task while Hermes is active; verify each touches only its own intended execution environment.
6. Stop/restart, connection loss and an account sign-in refresh. Confirm errors surface and no pending action is blindly replayed.

Only the offline native-agent subset has been automated in this branch. Real account/browser validation and the user's Mac deployment remain separate acceptance steps.

## Reproduce offline verification

From this fork, after installing Hermes's own dependencies in an isolated Python environment:

```bash
bun x --no-install --bun tsc --noEmit
bun test tests/hermes-config.test.ts tests/hermes-request.test.ts \
  tests/hermes-server.test.ts tests/hermes-mcp.test.ts tests/hermes-tool-loop.test.ts
node --test launcher/tests/*.test.cjs
PYTHONPATH=/path/to/pinned/hermes-agent python integrations/hermes/test_provider.py
HERMES_TEST_BUN="$(command -v bun)" PYTHONPATH=/path/to/pinned/hermes-agent \
  python integrations/hermes/test_agent_loop.py
```

`test_agent_loop.py` creates a temporary home/workspace and uses real Hermes tools. The ChatGPT model/browser is deterministic test code; it never reads account cookies or calls a paid model.

## Rollback

Stop the new `hermes serve` process and choose the previous provider in Hermes. The old Mac/Codex tunnel and files were not migrated. Do not delete the shared Electron profile: Mac may still use it. Remove the added plugin only after stopping processes that use it, and preserve logs/transcripts needed to reconcile unfinished tool actions.
