import { createHash, randomUUID } from "node:crypto";
import * as z from "zod/v4";
import { parseRequest } from "../responses/parser";
import type { CodexParsedRequest } from "../types";
import type { ChatGptTurnEnvironment, ChatGptTurnIdentity, ChatGptTurnUserRevision } from "../adapters/chatgpt-web/environment";

/** Created only after the dedicated HTTP boundary authenticates the request. Never deserialize this from the wire. */
export interface HermesTurnContext {
  identity: ChatGptTurnIdentity;
  revisions: ChatGptTurnUserRevision[];
  environment: ChatGptTurnEnvironment;
}

const name = z.string().regex(/^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,255}$/);
const image = z.object({ type: z.literal("image_url"), image_url: z.object({ url: z.string(), detail: z.enum(["auto", "low", "high"]).optional() }) });
const part = z.union([z.object({ type: z.literal("text"), text: z.string() }), image]);
const tool = z.object({ type: z.literal("function"), function: z.object({ name, description: z.string().optional(), parameters: z.record(z.string(), z.unknown()).optional(), strict: z.boolean().optional() }) });
const message = z.object({
  role: z.enum(["system", "developer", "user", "assistant", "tool"]),
  content: z.union([z.string(), z.array(part), z.null()]).optional(),
  tool_call_id: z.string().min(1).max(512).optional(),
  tool_calls: z.array(z.object({ id: z.string().min(1).max(512), type: z.literal("function"), function: z.object({ name, arguments: z.string() }) })).optional(),
  reasoning: z.string().optional(),
  reasoning_content: z.string().optional(),
});
const schema = z.object({
  model: z.string().min(1), messages: z.array(message).min(1).max(10_000),
  tools: z.array(tool).max(1_000).optional(),
  tool_choice: z.enum(["auto", "none"]).optional(),
  stream: z.boolean().optional(), stream_options: z.object({ include_usage: z.boolean().optional() }).optional(),
  parallel_tool_calls: z.boolean().optional(),
  hermes: z.object({ session_id: z.string().min(1).max(1024).optional(), profile_id: z.string().min(1).max(1024).optional() }).optional(),
  response_format: z.union([
    z.object({ type: z.literal("text") }),
    z.object({ type: z.literal("json_object") }),
    z.object({ type: z.literal("json_schema"), json_schema: z.object({ name: z.string().min(1), schema: z.record(z.string(), z.unknown()), strict: z.boolean().optional() }) }),
  ]).optional(),
  n: z.number().optional(),
});

function hash(value: unknown): string {
  return createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

function contentParts(content: z.infer<typeof message>["content"], assistant = false): unknown[] {
  if (content === undefined || content === null) return [];
  if (typeof content === "string") return [{ type: assistant ? "output_text" : "input_text", text: content }];
  return content.map(block => {
    if (block.type === "text") return { type: assistant ? "output_text" : "input_text", text: block.text };
    if (assistant) throw new Error("Hermes assistant history cannot contain image_url output");
    if (!/^data:image\/[A-Za-z0-9.+-]+;base64,/.test(block.image_url.url)) {
      let url: URL;
      try { url = new URL(block.image_url.url); } catch { throw new Error("Invalid Hermes image URL"); }
      if (url.protocol !== "https:" || url.username || url.password) throw new Error("Hermes images require HTTPS or an image data URL");
    }
    return { type: "input_image", image_url: block.image_url.url, ...(block.image_url.detail ? { detail: block.image_url.detail } : {}) };
  });
}

export function parseHermesRequest(value: unknown, namespace: string): CodexParsedRequest {
  if (!namespace) throw new Error("Hermes backend namespace is required");
  const body = schema.parse(value);
  if (body.n !== undefined && body.n !== 1) throw new Error("Hermes browser backend supports only n=1");
  if (value && typeof value === "object") {
    const raw = value as Record<string, unknown>;
    for (const field of ["audio", "modalities", "prediction", "functions", "function_call", "previous_response_id", "logprobs", "top_logprobs"]) {
      if (raw[field] != null) throw new Error(`Hermes browser backend does not support ${field}`);
    }
    if (raw.stop != null) throw new Error("Hermes browser backend does not implement stop sequences");
  }
  const tools = body.tool_choice === "none" ? [] : body.tools ?? [];
  if (new Set(tools.map(t => t.function.name)).size !== tools.length) throw new Error("Hermes tools contain duplicate names");
  if (tools.length && !body.hermes?.session_id) throw new Error("Tool-capable Hermes requests require hermes.session_id from the provider plugin");
  const threadId = hash(["hermes", namespace, body.hermes?.profile_id ?? "default", body.hermes?.session_id ?? randomUUID()]);
  const input: Array<Record<string, unknown>> = [];
  const revisions: ChatGptTurnUserRevision[] = [];
  const pending = new Set<string>();
  const seen = new Set<string>();
  // Use only human/task history for instruction lineage. Ephemeral system/budget layers do not create a new user turn.
  const taskPrefix: unknown[] = [];
  for (const m of body.messages) {
    if (m.role !== "assistant" && m.tool_calls?.length) throw new Error("Only assistant messages may contain tool_calls");
    if (m.role === "tool") {
      if (!m.tool_call_id || !pending.delete(m.tool_call_id)) throw new Error("Hermes history contains an orphan or duplicate tool result");
      input.push({ type: "function_call_output", call_id: m.tool_call_id, output: typeof m.content === "string" ? m.content : contentParts(m.content) });
      taskPrefix.push({ role: "tool", call_id: m.tool_call_id, content: m.content });
      continue;
    }
    if (m.tool_call_id !== undefined) throw new Error("Only tool messages may contain tool_call_id");
    const content = contentParts(m.content, m.role === "assistant");
    if (m.role !== "assistant" && !content.length) throw new Error("Hermes messages must contain task text or images");
    if (m.role === "user" && pending.size) throw new Error("Hermes history has missing tool results before a new user message");
    if (m.role !== "system" && m.role !== "developer") taskPrefix.push({ role: m.role, content: m.content ?? null });
    if (m.role === "user") {
      const itemId = `hermes_user_${hash(taskPrefix)}`;
      revisions.push({ itemId, content });
      input.push({ type: "message", role: m.role, id: itemId, content });
    } else if (content.length) {
      input.push({ type: "message", role: m.role, content });
    }
    if (m.role === "assistant") {
      const reasoning = m.reasoning_content ?? m.reasoning;
      if (reasoning) input.push({ type: "reasoning", summary: [{ type: "summary_text", text: reasoning }] });
      for (const call of m.tool_calls ?? []) {
        if (seen.has(call.id)) throw new Error("Hermes history contains duplicate tool call IDs");
        let args: unknown;
        try { args = JSON.parse(call.function.arguments); } catch { throw new Error("Hermes tool arguments are not valid JSON"); }
        if (!args || typeof args !== "object" || Array.isArray(args)) throw new Error("Hermes tool arguments must be a JSON object");
        seen.add(call.id);
        pending.add(call.id);
        input.push({ type: "function_call", call_id: call.id, name: call.function.name, arguments: call.function.arguments });
        taskPrefix.push({ role: "call", ...call });
      }
    }
  }
  if (!revisions.length) throw new Error("Hermes history requires a user message");
  if (pending.size) throw new Error("Hermes history has missing tool results");
  const format = body.response_format;
  const text = format?.type === "json_schema" ? { format: { type: "json_schema", ...format.json_schema } }
    : format?.type === "json_object" ? { format: { type: "json_schema", name: "hermes_json", strict: false, schema: { type: "object" } } }
    : undefined;
  const parsed = parseRequest({
    model: body.model, input, stream: body.stream === true,
    tools: tools.map(t => ({ type: "function", ...t.function })),
    tool_choice: body.tool_choice ?? "auto",
    parallel_tool_calls: body.parallel_tool_calls ?? true,
    ...(text ? { text } : {}),
  });
  const cwd = process.cwd();
  parsed.context.tools ??= [];
  parsed._hermes = {
    identity: { threadId, turnId: hash([threadId, revisions.at(-1)!.itemId]) },
    revisions,
    environment: { cwd, roots: [cwd], writableRoots: [], sandboxPolicy: { type: "external", executor: "hermes" }, tools: parsed.context.tools },
  };
  return parsed;
}
