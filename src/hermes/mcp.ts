import { createHash, randomUUID } from "node:crypto";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import * as z from "zod/v4";
import { callTurnBroker, type BrokerToolResult } from "../adapters/chatgpt-web/turn-broker";
import type { ChatGptTurnEnvironment } from "../adapters/chatgpt-web/environment";
import { observeMcpToolCalls } from "../adapters/chatgpt-web/mcp-observation";
import { VERSION } from "../version";

const MAX_OPERATIONS = 256;
const MAX_CACHED_BYTES = 32 * 1024 * 1024;
const TOKEN = z.string().min(20).max(256);
const OPERATION = z.string().regex(/^[A-Za-z0-9_-]{1,128}$/);
const result = (value: Record<string, unknown>, isError = false): BrokerToolResult => ({
  content: [{ type: "text", text: JSON.stringify(value) }], structuredContent: value, ...(isError ? { isError: true } : {}),
});
function digest(value: unknown): string { return createHash("sha256").update(JSON.stringify(value)).digest("hex"); }
function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => [k, canonical(v)]));
  return value;
}
function asMcp(value: BrokerToolResult) {
  return { content: value.content as never,
    ...(value.isError ? { isError: true } : {}),
    ...(value.structuredContent && typeof value.structuredContent === "object" && !Array.isArray(value.structuredContent)
      ? { structuredContent: value.structuredContent as Record<string, unknown> } : {}),
    ...(value._meta && typeof value._meta === "object" && !Array.isArray(value._meta)
      ? { _meta: value._meta as Record<string, unknown> } : {}),
  };
}

interface Claimed { bindingId: string; environment: ChatGptTurnEnvironment }
interface Operation { signature: string; controller: AbortController; promise: Promise<BrokerToolResult>; bytes: number }

/** Keeps tool execution in Hermes. Polling avoids holding the MCP tunnel open for long native tools. */
export function createHermesMcpServer(options: { brokerSocketPath: string; pollWaitMs?: number; toolTimeoutMs?: number }) {
  const pollWaitMs = options.pollWaitMs ?? 25_000;
  const toolTimeoutMs = options.toolTimeoutMs ?? 15 * 60_000;
  if (!Number.isFinite(pollWaitMs) || pollWaitMs < 1 || pollWaitMs > 25_000) throw new Error("Invalid Hermes MCP poll interval");
  if (!Number.isFinite(toolTimeoutMs) || toolTimeoutMs < pollWaitMs) throw new Error("Invalid Hermes tool deadline");
  const operations = new Map<string, Operation>();
  const scopes = new Map<string, { controller: AbortController; keys: Set<string> }>();
  let cachedBytes = 0;
  let closing = false;
  const server = new McpServer({ name: "hermes-chatgpt", version: VERSION }, { instructions:
    "These tools dispatch to the current Hermes Agent, not to Codex. Discover exact tool schemas, then invoke with a unique operation_id per intended action. A pending result is NOT completion. Poll codex_tool_call with the SAME turn_token and operation_id, omitting wire_name and arguments. Never resubmit a pending action under a new operation_id. Hermes alone owns approvals and execution." });

  const forget = (token: string) => {
    const scope = scopes.get(token);
    if (!scope) return;
    scopes.delete(token);
    scope.controller.abort();
    for (const key of scope.keys) {
      const op = operations.get(key);
      if (!op) continue;
      operations.delete(key);
      cachedBytes -= op.bytes;
      op.controller.abort();
    }
  };
  const watch = (token: string) => {
    if (scopes.has(token)) return scopes.get(token)!;
    const scope = { controller: new AbortController(), keys: new Set<string>() };
    scopes.set(token, scope);
    void callTurnBroker(options.brokerSocketPath, { method: "owner_wait_retirement", token }, null, scope.controller.signal)
      .then(() => forget(token), () => forget(token));
    return scope;
  };
  const settle = async (token: string, activityId: string) => {
    let failure: unknown;
    for (let attempt = 0; attempt < 2; attempt++) {
      try { await callTurnBroker(options.brokerSocketPath, { method: "activity_complete", token, activityId }, 5_000); return; }
      catch (error) { failure = error; }
    }
    throw failure;
  };
  const claimed = async <T>(token: string, action: (claim: Claimed) => Promise<T> | T): Promise<T> => {
    if (closing) throw new Error("Hermes MCP is closing");
    const activityId = `activity_${randomUUID()}`;
    try {
      const claim = await callTurnBroker<Claimed>(options.brokerSocketPath, { method: "claim", token, activityId, contract: "native" }, 5_000);
      if (claim.environment.sandboxPolicy.type !== "external" || claim.environment.sandboxPolicy.executor !== "hermes") {
        throw new Error("This capability does not belong to the Hermes executor");
      }
      return await action(claim);
    } finally { await settle(token, activityId); }
  };

  server.registerTool("codex_tool_inventory", {
    title: "Discover this Hermes Agent's tools",
    description: "Return exact tools advertised by the active Hermes turn, including its native memory, skills, delegation and configured plugin tools. No additional tools are granted.",
    inputSchema: { turn_token: TOKEN, query: z.string().max(500).optional(), offset: z.number().int().min(0).max(100_000).default(0), limit: z.number().int().min(1).max(50).default(20), include_schema: z.boolean().default(true) },
    annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
  }, async input => claimed(input.turn_token, claim => {
    const query = input.query?.toLowerCase() ?? "";
    const matches = claim.environment.tools.filter(t => `${t.name}\n${t.description}`.toLowerCase().includes(query));
    const page = matches.slice(input.offset, input.offset + input.limit);
    return asMcp(result({ total: matches.length, next_offset: input.offset + page.length < matches.length ? input.offset + page.length : null,
      tools: page.map(t => ({ wire_name: t.name, name: t.name, description: t.description, kind: "function", ...(input.include_schema ? { parameters: t.parameters } : {}) })) }));
  }));

  server.registerTool("codex_tool_call", {
    title: "Invoke or poll a native Hermes tool",
    description: "Start: turn_token, unique operation_id, exact wire_name, arguments. Pending: poll with the SAME turn_token and operation_id only. Retries with the same ID never execute twice in this live MCP process; changing arguments under an existing ID is rejected. Do not interpret pending as success. Tool execution and approvals stay in Hermes.",
    inputSchema: { turn_token: TOKEN, operation_id: OPERATION, wire_name: z.string().min(1).max(256).optional(), arguments: z.record(z.string(), z.unknown()).optional() },
    annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: true, openWorldHint: true },
  }, async input => claimed(input.turn_token, async claim => {
    const key = digest([input.turn_token, input.operation_id]);
    const signature = input.wire_name ? digest([input.wire_name, canonical(input.arguments ?? {})]) : undefined;
    let op = operations.get(key);
    if (op && signature && op.signature !== signature) throw new Error("Hermes operation_id was reused with different arguments");
    if (!input.wire_name && input.arguments !== undefined) throw new Error("Polling a Hermes operation must omit arguments");
    if (!op) {
      if (!input.wire_name) throw new Error("Unknown Hermes operation_id in this turn");
      const tool = claim.environment.tools.find(t => t.name === input.wire_name && !t.namespace && !t.freeform && !t.toolSearch);
      if (!tool) throw new Error("Hermes tool is not advertised in this turn");
      if (operations.size >= MAX_OPERATIONS) throw new Error("Hermes MCP operation capacity reached; finish or cancel active turns");
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(new Error("Hermes tool exceeded its execution deadline")), toolTimeoutMs);
      timer.unref?.();
      const operation: Operation = { signature: signature!, controller, bytes: 0, promise: Promise.resolve(result({ pending: true })) };
      operations.set(key, operation);
      watch(input.turn_token).keys.add(key);
      operation.promise = callTurnBroker<BrokerToolResult>(options.brokerSocketPath, {
        method: "invoke", bindingId: claim.bindingId, wireName: tool.name, freeform: false, arguments: input.arguments ?? {},
      }, null, controller.signal).then(value => {
        const bytes = Buffer.byteLength(JSON.stringify(value));
        if (cachedBytes + bytes > MAX_CACHED_BYTES) return result({ error: "Hermes tool completed, but its result exceeds the MCP cache limit. Do not repeat the side effect." }, true);
        // Retirement may have removed this entry while the native result was in flight.
        if (operations.get(key) === operation) { operation.bytes = bytes; cachedBytes += bytes; }
        return value;
      }).catch(async error => {
        await callTurnBroker(options.brokerSocketPath, { method: "release", bindingId: claim.bindingId }, 5_000).catch(() => {});
        return result({ error: error instanceof Error ? error.message : String(error), retryable: false, message: "Hermes tool execution did not settle; the capability was retired. Do not replay the action automatically." }, true);
      }).finally(() => clearTimeout(timer));
      op = operation;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const pending = new Promise<BrokerToolResult>(resolve => { timer = setTimeout(() => resolve(result({ pending: true, operation_id: input.operation_id, message: "Poll this same operation_id; do not resubmit the action." })), pollWaitMs); });
      return asMcp(await Promise.race([op.promise, pending]));
    } finally { clearTimeout(timer); }
  }));

  return { server, async close() {
    closing = true;
    const pending = [...operations.values()].map(op => op.promise);
    for (const token of [...scopes.keys()]) forget(token);
    await Promise.all(pending);
    await server.close();
  } };
}

export async function runHermesMcpServer(brokerSocketPath: string): Promise<void> {
  const host = createHermesMcpServer({ brokerSocketPath });
  await host.server.connect(observeMcpToolCalls(new StdioServerTransport(), new Set(["codex_tool_inventory", "codex_tool_call"])));
}
