// Offline contract fixture: production HTTP/adapter/MCP/broker, deterministic browser only.
// Never reads login profiles or connects to ChatGPT. Invoked by test_agent_loop.py.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { join } from "node:path";
import { defaultConfig } from "../../src/config";
import { createChatGptWebAdapter } from "../../src/adapters/chatgpt-web";
import { ChatGptBrowserWorker } from "../../src/adapters/chatgpt-web/browser-worker";
import { createHermesMcpServer } from "../../src/hermes/mcp";
import { startHermesServer } from "../../src/hermes/server";

const root = process.argv[2];
const apiToken = process.env.HERMES_CHATGPT_TEST_KEY;
if (!root || !apiToken) throw new Error("Test root and test key required");
const brokerSocketPath = join(root, "broker.sock");
const mcp = createHermesMcpServer({ brokerSocketPath, pollWaitMs: 10 });
const [a, b] = InMemoryTransport.createLinkedPair();
await mcp.server.connect(a);
const client = new Client({ name: "offline-browser-fixture", version: "1" });
await client.connect(b);
let browserStarts = 0;
const backend = await startHermesServer({
  namespace: "hermes-native-loop-contract", apiToken,
  runtime: { ...defaultConfig("full"), port: 0, brokerSocketPath, appName: "Hermes ChatGPT", automaticAppName: "Hermes ChatGPT", solAvailable: true },
}, { adapterFactory(provider) {
  const worker = ChatGptBrowserWorker.forProvider(provider);
  worker.run = async turn => {
    const ordinal = ++browserStarts;
    const prepared = await turn.prepare();
    try {
      const token = prepared.text.match(/turn_token (turn_[A-Za-z0-9_-]+)/)?.[1];
      if (!token) throw new Error("Hermes native loop did not supply its capability");
      if (turn.conversationKey !== undefined) throw new Error("Hermes unexpectedly retained hidden browser context");
      const progress = turn.externalProgress!;
      const inventory = await client.callTool({ name: "codex_tool_inventory", arguments: { turn_token: token, limit: 50 } });
      if (inventory.isError) throw new Error(JSON.stringify(inventory));
      const advertised = new Set((inventory.structuredContent as { tools: Array<{ name: string }> }).tools.map(t => t.name));
      const desired: Array<[string, Record<string, unknown>]> = [
        ["write_file", { path: join(root, "proof.txt"), content: "HERMES_NATIVE_FILE" }],
        ["read_file", { path: join(root, "proof.txt") }],
        ["terminal", { command: "printf HERMES_NATIVE_TERMINAL", timeout: 10 }],
        ["todo_list", { todos: [{ id: "verified", content: "Native tool loop", status: "completed" }] }],
        ["memory", { action: "add", target: "memory", content: "HERMES_BACKEND_TEST_ONLY" }],
        ["terminal", { command: `touch ${join(root, "denied.txt")}`, timeout: 10 }],
      ];
      const actions: Array<[string, Record<string, unknown>]> = [];
      for (const [name, args] of desired) {
        if (advertised.has(name)) actions.push([name, args]);
        else if (advertised.has("tool_describe") && advertised.has("tool_call")) {
          // Deferred tools stay behind Hermes's own discovery/dispatch boundary.
          actions.push(["tool_describe", { names: [name] }]);
          actions.push(["tool_call", { calls: [{ name, arguments: args }] }]);
        } else throw new Error(`Hermes did not advertise ${name} or its discovery gateway`);
      }
      for (const [index, [name, args]] of actions.entries()) {
        const operation_id = `native-${ordinal}-${index}`;
        const before = progress.snapshot().lastToolBatchRevision;
        const pending = client.callTool({ name: "codex_tool_call", arguments: { turn_token: token, operation_id, wire_name: name, arguments: args } });
        const acknowledge = async () => {
          let snapshot = progress.snapshot();
          while (snapshot.lastToolBatchRevision <= before) snapshot = await progress.waitForChange(snapshot.revision, turn.abortSignal);
          await progress.acknowledgeToolBatch(snapshot.lastToolBatchRevision);
        };
        const boundary = acknowledge();
        await Promise.race([boundary, pending.then(result => {
          if (result.isError) throw new Error(JSON.stringify(result));
          return boundary;
        })]);
        let result = await pending;
        while ((result.structuredContent as { pending?: boolean } | undefined)?.pending) {
          result = await client.callTool({ name: "codex_tool_call", arguments: { turn_token: token, operation_id } });
        }
        const denialProbe = name === "terminal" && String(args.command).startsWith("touch ");
        if (denialProbe) {
          if (!JSON.stringify(result).includes("BLOCKED")) throw new Error("Hermes approval policy did not reject the denied command");
        } else if (result.isError) throw new Error(`Native ${name} failed: ${JSON.stringify(result)}`);
        if (name === "read_file" && !JSON.stringify(result).includes("HERMES_NATIVE_FILE")) throw new Error("Native file result did not reach the browser");
        if (name === "terminal" && !denialProbe && !JSON.stringify(result).includes("HERMES_NATIVE_TERMINAL")) throw new Error("Native terminal result did not reach the browser");
      }
      const answer = `HERMES_NATIVE_LOOP_OK browser_starts=${ordinal}`;
      turn.onTextDelta(answer);
      return answer;
    } finally { prepared.release(); }
  };
  return createChatGptWebAdapter(provider);
} });
console.log(`HERMES_TEST_PORT=${backend.server.port}`);
await new Promise<void>(resolve => {
  process.once("SIGTERM", resolve);
  process.once("SIGINT", resolve);
});
await backend.close();
await client.close();
await mcp.close();
