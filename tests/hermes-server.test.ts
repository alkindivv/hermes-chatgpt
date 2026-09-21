import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defaultConfig } from "../src/config";
import { startHermesServer } from "../src/hermes/server";
import type { ProviderAdapter } from "../src/adapters/base";
import type { AdapterEvent } from "../src/types";

const close: Array<() => Promise<void>> = [];
const roots: string[] = [];
afterEach(async () => { for (const action of close.splice(0)) await action(); for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });
const token = "test-hermes-token-" + "s".repeat(48);
function config() {
  const root = mkdtempSync(join(tmpdir(), "hermes-server-"));
  roots.push(root);
  return { runtime: { ...defaultConfig("full"), brokerSocketPath: join(root, "broker.sock"), port: 0, appName: "Hermes ChatGPT", automaticAppName: "Hermes ChatGPT", solAvailable: true, extraHighAvailable: false, proAvailable: false }, namespace: "test-backend", apiToken: token };
}
function adapter(events: AdapterEvent[], observer?: () => void): ProviderAdapter {
  return { name: "test", async runTurn(_parsed, _incoming, emit) { observer?.(); for (const event of events) emit(event); } };
}
function send(base: string, body: unknown, auth: string | null = token) {
  return fetch(`${base}/v1/chat/completions`, { method: "POST", headers: { ...(auth ? { authorization: `Bearer ${auth}` } : {}), "content-type": "application/json" }, body: JSON.stringify(body) });
}
const prompt = { model: "chatgpt-web/high", messages: [{ role: "user", content: "Hello" }] };

test("Hermes endpoint authenticates before dispatch and catalog never needs Codex credentials", async () => {
  let called = 0;
  const host = await startHermesServer(config(), { adapterFactory: () => adapter([{ type: "text_delta", text: "hello" }, { type: "done", stopReason: "stop" }], () => called++) });
  close.push(host.close);
  const base = `http://127.0.0.1:${host.server.port}`;
  expect((await send(base, prompt, null)).status).toBe(401);
  expect((await send(base, prompt, "wrong")).status).toBe(401);
  expect(called).toBe(0);
  const list = await fetch(`${base}/v1/models`, { headers: { authorization: `Bearer ${token}` } });
  const models = await list.json() as any;
  expect(models.object).toBe("list");
  expect(models.data.map((m: any) => m.id)).toEqual(["chatgpt-web/light", "chatgpt-web/medium", "chatgpt-web/high"]);
  expect((await send(base, { ...prompt, model: "gpt-5.6-sol" })).status).toBe(400);
  expect(called).toBe(0);
  const result = await send(base, prompt);
  expect(result.status).toBe(200);
  expect(await result.json()).toMatchObject({ object: "chat.completion", choices: [{ index: 0, message: { role: "assistant", content: "hello" }, finish_reason: "stop" }] });
  expect(called).toBe(1);
});

test("verified model catalog proves browser health when idle", async () => {
  let inspected: string[] = [];
  const cfg = config();
  cfg.runtime.browserHost = "launcher";
  cfg.runtime.browserHostDescriptorPath = "/test/launcher-browser.json";
  const host = await startHermesServer(cfg, {
    adapterFactory: () => adapter([{ type: "text_delta", text: "unused" }, { type: "done", stopReason: "stop" }]),
    inspectBrowser: async path => { inspected.push(path); },
  });
  close.push(host.close);
  const base = `http://127.0.0.1:${host.server.port}`;
  const response = await fetch(`${base}/v1/models/verified`, {
    headers: { authorization: `Bearer ${token}` },
  });
  expect(response.status).toBe(200);
  expect((await response.json() as any).data.map((m: any) => m.id))
    .toEqual(["chatgpt-web/light", "chatgpt-web/medium", "chatgpt-web/high"]);
  expect(inspected).toEqual(["/test/launcher-browser.json"]);
});

test("verified model catalog reports browser health failures instead of a false healthy catalog", async () => {
  const cfg = config();
  cfg.runtime.browserHost = "launcher";
  cfg.runtime.browserHostDescriptorPath = "/test/launcher-browser.json";
  const host = await startHermesServer(cfg, {
    adapterFactory: () => adapter([{ type: "done", stopReason: "stop" }]),
    inspectBrowser: async () => { throw new Error("ChatGPT session expired"); },
  });
  close.push(host.close);
  const response = await fetch(
    `http://127.0.0.1:${host.server.port}/v1/models/verified`,
    { headers: { authorization: `Bearer ${token}` } },
  );
  expect(response.status).toBe(503);
  expect(await response.json()).toMatchObject({
    error: {
      type: "server_error",
      code: "chatgpt_browser_unavailable",
      retryable: true,
      message: "ChatGPT session expired",
    },
  });
});

test("streaming preserves reasoning, text, indexed tool arguments and usage without successful error masking", async () => {
  const events: AdapterEvent[] = [
    { type: "heartbeat" }, { type: "thinking_delta", thinking: "Visible reasoning summary" },
    { type: "text_delta", text: "Checking" },
    { type: "tool_call_start", id: "call_native", name: "read_file" },
    { type: "tool_call_delta", arguments: '{"path":' }, { type: "tool_call_delta", arguments: '"a.txt"}' }, { type: "tool_call_end" },
    { type: "done", stopReason: "tool_use", endTurn: false, usage: { inputTokens: 100, outputTokens: 20 } },
  ];
  const host = await startHermesServer(config(), { adapterFactory: () => adapter(events) });
  close.push(host.close);
  const response = await send(`http://127.0.0.1:${host.server.port}`, { ...prompt, stream: true, stream_options: { include_usage: true } });
  const text = await response.text();
  const frames = text.split("\n").filter(line => line.startsWith("data: ") && !line.includes("[DONE]")).map(line => JSON.parse(line.slice(6)));
  expect(frames.some(f => f.choices[0]?.delta.reasoning_content === "Visible reasoning summary")).toBe(true);
  expect(frames.some(f => f.choices[0]?.delta.tool_calls?.[0]?.id === "call_native")).toBe(true);
  expect(frames.some(f => f.choices[0]?.finish_reason === "tool_calls")).toBe(true);
  expect(frames.at(-1).usage).toMatchObject({ prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 });
  expect(text).toContain("data: [DONE]");
});

test("parallel tool batches retain independent ids and OpenAI finish semantics", async () => {
  const events: AdapterEvent[] = [
    { type: "tool_call_start", id: "call_a", name: "read_file" },
    { type: "tool_call_delta", arguments: '{"path":"a.txt"}' },
    { type: "tool_call_end" },
    { type: "tool_call_start", id: "call_b", name: "read_file" },
    { type: "tool_call_delta", arguments: '{"path":"b.txt"}' },
    { type: "tool_call_end" },
    { type: "done", stopReason: "tool_use", endTurn: false },
  ];
  const host = await startHermesServer(config(), { adapterFactory: () => adapter(events) });
  close.push(host.close);
  const response = await send(`http://127.0.0.1:${host.server.port}`, {
    ...prompt,
    parallel_tool_calls: true,
  });
  expect(response.status).toBe(200);
  const body = await response.json() as any;
  expect(body.choices[0].finish_reason).toBe("tool_calls");
  expect(body.choices[0].message.tool_calls).toEqual([
    {
      id: "call_a",
      type: "function",
      function: { name: "read_file", arguments: '{"path":"a.txt"}' },
    },
    {
      id: "call_b",
      type: "function",
      function: { name: "read_file", arguments: '{"path":"b.txt"}' },
    },
  ]);
});

test("structured output contract reaches the adapter without changing native model identity", async () => {
  let observed: any;
  const host = await startHermesServer(config(), {
    adapterFactory: () => ({
      name: "structured",
      async runTurn(parsed, _incoming, emit) {
        observed = parsed.options.outputFormat;
        emit({ type: "text_delta", text: '{"ok":true}' });
        emit({ type: "done", stopReason: "stop" });
      },
    }),
  });
  close.push(host.close);
  const response = await send(`http://127.0.0.1:${host.server.port}`, {
    ...prompt,
    response_format: {
      type: "json_schema",
      json_schema: {
        name: "result",
        strict: true,
        schema: {
          type: "object",
          properties: { ok: { type: "boolean" } },
          required: ["ok"],
          additionalProperties: false,
        },
      },
    },
  });
  expect(response.status).toBe(200);
  expect(observed).toMatchObject({ type: "json_schema", name: "result", strict: true });
  expect((await response.json() as any).model).toBe("chatgpt-web/high");
});

test("upstream errors stay structured for JSON and SSE and incomplete output is not reported as stop", async () => {
  const host = await startHermesServer(config(), { adapterFactory: () => adapter([{
    type: "error",
    message: "Session expired",
    status: 401,
    errorType: "authentication_error",
    code: "chatgpt_session_expired",
    retryable: false,
  }]) });
  close.push(host.close);
  const base = `http://127.0.0.1:${host.server.port}`;
  const response = await send(base, prompt);
  expect(response.status).toBe(401);
  expect(await response.json()).toMatchObject({
    error: {
      message: "Session expired",
      type: "authentication_error",
      code: "chatgpt_session_expired",
      retryable: false,
    },
  });
  const stream = await (await send(base, { ...prompt, stream: true })).text();
  expect(stream).toContain('"error"');
  expect(stream).toContain('"type":"authentication_error"');
  expect(stream).toContain('"code":"chatgpt_session_expired"');
  expect(stream).toContain('"retryable":false');
  expect(stream).not.toContain('"finish_reason":"stop"');
});

test("client stream cancellation aborts the owned browser turn", async () => {
  let began!: () => void;
  const started = new Promise<void>(resolve => { began = resolve; });
  let aborted!: () => void;
  const sawAbort = new Promise<void>(resolve => { aborted = resolve; });
  const host = await startHermesServer(config(), {
    adapterFactory: () => ({
      name: "client-cancel",
      async runTurn(_parsed, incoming, emit) {
        began();
        emit({ type: "heartbeat" });
        await new Promise<void>(resolve => incoming.abortSignal?.addEventListener("abort", () => {
          aborted();
          resolve();
        }, { once: true }));
      },
    }),
  });
  close.push(host.close);
  const response = await send(
    `http://127.0.0.1:${host.server.port}`,
    { ...prompt, stream: true },
  );
  await started;
  const reader = response.body!.getReader();
  await reader.cancel();
  await Promise.race([
    sawAbort,
    new Promise((_, reject) => setTimeout(() => reject(new Error("cancel did not propagate")), 2_000)),
  ]);
});

test("closing the backend aborts active requests", async () => {
  let began!: () => void;
  const started = new Promise<void>(resolve => { began = resolve; });
  let aborted = false;
  const host = await startHermesServer(config(), { adapterFactory: () => ({ name: "waiting", async runTurn(_parsed, incoming) {
    began();
    await new Promise<void>(resolve => incoming.abortSignal?.addEventListener("abort", () => { aborted = true; resolve(); }, { once: true }));
  } }) });
  const pending = send(`http://127.0.0.1:${host.server.port}`, { ...prompt, stream: true }).then(r => r.text()).catch(() => "aborted");
  await started;
  await host.close();
  await pending;
  expect(aborted).toBe(true);
});
