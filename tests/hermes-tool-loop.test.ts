import { expect, test } from "bun:test";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defaultConfig } from "../src/config";
import { startHermesServer } from "../src/hermes/server";
import { createHermesMcpServer } from "../src/hermes/mcp";
import { createChatGptWebAdapter } from "../src/adapters/chatgpt-web";
import { ChatGptBrowserWorker } from "../src/adapters/chatgpt-web/browser-worker";

test("real HTTP -> browser adapter -> MCP broker -> outer tool result -> same browser response", async () => {
  const root = mkdtempSync(join(tmpdir(), "hermes-loop-"));
  const brokerSocketPath = join(root, "b.sock");
  const apiToken = "k".repeat(64);
  const runtime = { ...defaultConfig("full"), brokerSocketPath, port: 0, appName: "Hermes ChatGPT", automaticAppName: "Hermes ChatGPT", solAvailable: true };
  const mcp = createHermesMcpServer({ brokerSocketPath, pollWaitMs: 2 });
  const [a, b] = InMemoryTransport.createLinkedPair();
  await mcp.server.connect(a);
  const client = new Client({ name: "simulated-browser-connector", version: "1" });
  await client.connect(b);
  let browserStarts = 0;
  const originals = new Map<ChatGptBrowserWorker, ChatGptBrowserWorker["run"]>();
  const backend = await startHermesServer({ runtime, namespace: "real-loop", apiToken }, { adapterFactory(provider) {
    const worker = ChatGptBrowserWorker.forProvider(provider);
    if (!originals.has(worker)) originals.set(worker, worker.run);
    worker.run = async turn => {
      browserStarts++;
      const prepared = await turn.prepare();
      try {
        const token = prepared.text.match(/turn_token (turn_[A-Za-z0-9_-]+)/)?.[1];
        if (!token) throw new Error("Missing native capability token");
        expect(prepared.text).toContain("Hermes");
        expect(turn.conversationKey).toBeUndefined();
        const progress = turn.externalProgress!;
        const before = progress.snapshot().lastToolBatchRevision;
        const invocation = client.callTool({ name: "codex_tool_call", arguments: { turn_token: token, operation_id: "read-one", wire_name: "read_file", arguments: { path: "proof.txt" } } });
        let snapshot = progress.snapshot();
        while (snapshot.lastToolBatchRevision <= before) snapshot = await progress.waitForChange(snapshot.revision, turn.abortSignal);
        await progress.acknowledgeToolBatch(snapshot.lastToolBatchRevision);
        let output = await invocation;
        while ((output.structuredContent as { pending?: boolean } | undefined)?.pending) {
          output = await client.callTool({ name: "codex_tool_call", arguments: { turn_token: token, operation_id: "read-one" } });
        }
        expect(output.isError).not.toBe(true);
        expect(JSON.stringify(output)).toContain("NATIVE_HERMES_RESULT");
        const answer = "Verified NATIVE_HERMES_RESULT";
        turn.onTextDelta(answer);
        return answer;
      } finally { prepared.release(); }
    };
    return createChatGptWebAdapter(provider);
  } });
  try {
    const send = (body: unknown) => fetch(`http://127.0.0.1:${backend.server.port}/v1/chat/completions`, { method: "POST", headers: { authorization: `Bearer ${apiToken}`, "content-type": "application/json" }, body: JSON.stringify(body) }).then(async r => {
      const data = await r.json() as any;
      if (!r.ok) throw new Error(JSON.stringify(data));
      return data;
    });
    const initial = { model: "chatgpt-web/high", hermes: { profile_id: "A", session_id: "native-Hermes-session" },
      messages: [{ role: "system", content: "You are Hermes. Use the native tools." }, { role: "user", content: "Read proof.txt" }],
      tools: [{ type: "function", function: { name: "read_file", description: "Read using the active native Hermes backend", parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } } }],
    };
    const first = await send(initial);
    expect(first.choices[0].finish_reason).toBe("tool_calls");
    const call = first.choices[0].message.tool_calls[0];
    expect(call.function.name).toBe("read_file");
    expect(JSON.parse(call.function.arguments)).toEqual({ path: "proof.txt" });
    // The outer agent performs the action, not the TypeScript bridge or browser.
    writeFileSync(join(root, "proof.txt"), "NATIVE_HERMES_RESULT");
    const nativeResult = readFileSync(join(root, JSON.parse(call.function.arguments).path), "utf8");
    const final = await send({ ...initial, messages: [...initial.messages, first.choices[0].message,
      { role: "tool", tool_call_id: call.id, content: nativeResult }] });
    expect(final.choices[0].finish_reason).toBe("stop");
    expect(final.choices[0].message.content).toContain("Verified NATIVE_HERMES_RESULT");
    expect(browserStarts).toBe(1);
  } finally {
    await client.close();
    await mcp.close();
    await backend.close();
    for (const [worker, run] of originals) worker.run = run;
    rmSync(root, { recursive: true, force: true });
  }
}, 20_000);
