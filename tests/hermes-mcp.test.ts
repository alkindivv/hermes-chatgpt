import { expect, test } from "bun:test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHermesMcpServer } from "../src/hermes/mcp";
import { TurnBroker } from "../src/adapters/chatgpt-web/turn-broker";
import type { ChatGptTurnEnvironment } from "../src/adapters/chatgpt-web/environment";

async function fixture() {
  const root = mkdtempSync(join(tmpdir(), "hermes-mcp-"));
  const broker = TurnBroker.forSocket(join(root, "b.sock"));
  const environment: ChatGptTurnEnvironment = { cwd: root, roots: [root], writableRoots: [], sandboxPolicy: { type: "external", executor: "hermes" }, tools: [
    { name: "memory", description: "Hermes native memory", parameters: { type: "object", properties: { text: { type: "string" } } } },
    { name: "delegate_task", description: "Hermes delegation", parameters: { type: "object" } },
  ] };
  const token = await broker.register(environment, 60_000);
  const host = createHermesMcpServer({ brokerSocketPath: broker.socketPath, pollWaitMs: 1 });
  const [serverTransport, clientTransport] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "test", version: "1" });
  await host.server.connect(serverTransport);
  await client.connect(clientTransport);
  return { token, client, broker, environment, async close() { await client.close(); await host.close(); await broker.close(); rmSync(root, { recursive: true, force: true }); } };
}

test("Hermes MCP exposes only native-registry inventory and dispatch, never Codex filesystem wrappers", async () => {
  const f = await fixture();
  try {
    expect((await f.client.listTools()).tools.map(t => t.name).sort()).toEqual(["codex_tool_call", "codex_tool_inventory"]);
    const inventory = await f.client.callTool({ name: "codex_tool_inventory", arguments: { turn_token: f.token } });
    expect(inventory.structuredContent).toMatchObject({ tools: [{ wire_name: "memory" }, { wire_name: "delegate_task" }], total: 2 });
    const mac = await f.broker.register({ ...f.environment, sandboxPolicy: { type: "dangerFullAccess" } }, 60_000);
    const denied = await f.client.callTool({ name: "codex_tool_inventory", arguments: { turn_token: mac } });
    expect(denied.isError).toBe(true);
    expect(JSON.stringify(denied)).toContain("Hermes");
  } finally { await f.close(); }
});

test("long-running native tools are polled without resubmission and result IDs stay owned", async () => {
  const f = await fixture();
  try {
    const args = { turn_token: f.token, operation_id: "memory-one", wire_name: "memory", arguments: { text: "Remember this" } };
    const pendingCall = f.client.callTool({ name: "codex_tool_call", arguments: args });
    const [queued] = await f.broker.nextToolBatch(f.token);
    expect(queued).toMatchObject({ wireName: "memory", arguments: { text: "Remember this" } });
    expect((await pendingCall).structuredContent).toMatchObject({ pending: true, operation_id: "memory-one" });
    const pendingAgain = await f.client.callTool({ name: "codex_tool_call", arguments: args });
    expect(pendingAgain.structuredContent).toMatchObject({ pending: true });
    const conflict = await f.client.callTool({ name: "codex_tool_call", arguments: { ...args, arguments: { text: "Changed side effect" } } });
    expect(conflict.isError).toBe(true);
    f.broker.completeTool(f.token, queued.callId, { content: [{ type: "text", text: "MEMORY_SAVED" }] });
    const result = await f.client.callTool({ name: "codex_tool_call", arguments: { turn_token: f.token, operation_id: "memory-one" } });
    expect(result.content).toEqual([{ type: "text", text: "MEMORY_SAVED" }]);
    const replay = await f.client.callTool({ name: "codex_tool_call", arguments: args });
    expect(replay.content).toEqual(result.content);
    const other = await f.broker.register(f.environment, 60_000);
    const stolen = await f.client.callTool({ name: "codex_tool_call", arguments: { turn_token: other, operation_id: "memory-one" } });
    expect(stolen.isError).toBe(true);
    const abort = new AbortController();
    const extra = f.broker.nextToolBatch(f.token, abort.signal);
    abort.abort();
    await expect(extra).rejects.toMatchObject({ name: "AbortError" });
  } finally { await f.close(); }
});
