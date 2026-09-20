# Hermes backend design

## Goal
Use this fork's existing authenticated ChatGPT browser and MCP turn broker as the model transport for NousResearch Hermes Agent, while Hermes retains its native agent loop, tools, approvals, memory, skills, terminal backends, delegation and transcript. Preserve the existing Mac/Codex integration unchanged.

## Reference
Official Hermes checkout: 8b42b6e020c3f5b50c555625c81eccf1bb97de56. Fork base: 432440e. The official model-provider plugin mechanism is the integration seam. Do not patch the user's Hermes installation or change existing profiles/services automatically.

## Architecture
An out-of-tree model-provider plugin declares the Chat Completions transport and adds profile/session provenance using the supported build_extra_body hook. A new authenticated loopback HTTP endpoint (default 17842) adapts those requests to the existing browser adapter and emits standard Chat Completions responses/tool calls. Tool execution always returns to Hermes's normal loop, never to a second registry runner in this bridge.

A dedicated Hermes bridge home, API token, broker socket, MCP tunnel and connector (`Hermes ChatGPT`) isolate Hermes capabilities from the Mac connector (`Codex Native2`). The physical Electron host may be shared; its existing tab/account limits remain unchanged. Browser discovery must search for the configured connector instead of hardcoding @codex.

## Identity and authority
Trusted Hermes identity is attached only inside the authenticated server after parsing. Raw request fields or user-authored XML never become Codex environment authority. The profile-scoped session and transcript prefix through the latest user message identify a logical turn; tool-result rounds preserve it, repeated later user requests do not collide. Auxiliary requests without tools may be stateless. Tool-capable requests without session provenance fail closed.

Hermes owns the actual terminal backend and permissions. Represent this explicitly as an external Hermes executor, not a fabricated Codex unrestricted sandbox. The MCP surface for this executor exposes only inventory and generic tool dispatch, not Codex-specific command/patch wrappers. Compatibility MCP wire names may remain `codex_tool_inventory` and `codex_tool_call`; their tool registry is the authenticated Hermes request's exact registry.

No browser conversation is retained between completed Hermes user turns. Each fresh user turn receives Hermes's canonical complete history; an active tool cycle still remains in one browser response. This deliberately avoids hidden browser-state reuse after Hermes compression or memory changes.

## Supported scope
Automatic Sol-backed ChatGPT Web routes with fixed effort (Instant/Medium/High/Extra High/Pro as the account permits). Text, advertised function tools, image parts, streaming and non-streaming completions, JSON-schema output where existing validation supports it. No native Codex OAuth passthrough, Zero Risk/manual mode, audio, arbitrary model aliases or automatic fallback to paid API routes. Luna's Codex-specific rolling checkpoint is not enabled for this initial Hermes backend.

## Setup and safety
Dedicated `hermes setup`, `hermes serve`, `hermes tunnel`, `hermes status` commands use an isolated home. They never install a Codex route, launch macOS services, edit Hermes config.yaml, or start/stop the Mac tunnel. Generate an owner-only API token file; never print credentials. Tunnel creation requires an explicitly supplied distinct tunnel ID and runtime-key file. Login uses the existing Electron UI. A plugin installation script refuses to overwrite an existing destination.

## Validation
Prove request identity stability, profile separation, exact tool IDs/arguments/results, no XML-based permissions, auth before side effects, output/error/cancellation semantics, and retained-context avoidance. Exercise real broker round trips and actual Hermes plugin discovery/transport imports in temporary homes. Run existing runtime and launcher suites plus builds. Account-bound browser/MCP execution and physical Mac deployment are separate live acceptance checks and must not be claimed on unit-test evidence.
