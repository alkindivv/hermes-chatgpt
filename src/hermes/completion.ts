import { randomUUID } from "node:crypto";
import type { AdapterEvent, CodexUsage } from "../types";

export class HermesCompletionError extends Error {
  constructor(
    message: string,
    readonly status = 502,
    readonly code = "hermes_backend_error",
    readonly errorType = "server_error",
    readonly retryable?: boolean,
  ) { super(message); }
}

export function completionError(error: unknown): {
  error: { message: string; type: string; code: string; retryable?: boolean };
} {
  const structured = error instanceof HermesCompletionError ? error : undefined;
  return {
    error: {
      message: error instanceof Error ? error.message : String(error),
      type: structured?.errorType ?? "server_error",
      code: structured?.code ?? "hermes_backend_error",
      ...(structured?.retryable === undefined ? {} : { retryable: structured.retryable }),
    },
  };
}

function usage(value?: CodexUsage) {
  const input = value?.inputTokens ?? 0;
  const output = value?.outputTokens ?? 0;
  return { prompt_tokens: input, completion_tokens: output, total_tokens: input + output,
    prompt_tokens_details: { cached_tokens: value?.cachedInputTokens ?? 0 } };
}

type ToolCall = { id: string; type: "function"; function: { name: string; arguments: string } };

/** One reducer is shared by JSON and SSE, including validation of terminal/tool states. */
export class HermesCompletion {
  readonly id = `chatcmpl-${randomUUID()}`;
  readonly created = Math.floor(Date.now() / 1000);
  private content = "";
  private reasoning = "";
  private calls: ToolCall[] = [];
  private currentTool: number | undefined;
  private terminal: "stop" | "tool_calls" | "length" | undefined;
  private consumed?: CodexUsage;
  private bytes = 0;

  constructor(readonly model: string, private readonly parallel = true) {}

  chunk(delta: Record<string, unknown>, finish: string | null = null) {
    return { id: this.id, object: "chat.completion.chunk", created: this.created, model: this.model,
      choices: [{ index: 0, delta, finish_reason: finish }] };
  }

  accept(event: AdapterEvent): Record<string, unknown> | undefined {
    if (event.type === "error") {
      throw new HermesCompletionError(
        event.message,
        event.status ?? 502,
        event.code,
        event.errorType ?? "server_error",
        event.retryable,
      );
    }
    if (this.terminal) throw new HermesCompletionError("Adapter emitted data after completion");
    if (event.type === "text_delta" || event.type === "thinking_delta" || event.type === "tool_call_delta") {
      const text = event.type === "text_delta" ? event.text : event.type === "thinking_delta" ? event.thinking : event.arguments;
      this.bytes += Buffer.byteLength(text);
      if (this.bytes > 16 * 1024 * 1024) throw new HermesCompletionError("Hermes completion exceeds the output size limit");
    }
    switch (event.type) {
      case "text_delta": this.content += event.text; return this.chunk({ content: event.text });
      case "thinking_delta": this.reasoning += event.thinking; return this.chunk({ reasoning_content: event.thinking });
      case "tool_call_start": {
        if (this.currentTool !== undefined || this.calls.some(c => c.id === event.id)) throw new HermesCompletionError("Invalid duplicate or overlapping tool call");
        if (!this.parallel && this.calls.length) throw new HermesCompletionError("Model returned parallel tools while disabled");
        const index = this.calls.length;
        const call: ToolCall = { id: event.id, type: "function", function: { name: event.name, arguments: "" } };
        this.calls.push(call);
        this.currentTool = index;
        return this.chunk({ tool_calls: [{ index, ...call }] });
      }
      case "tool_call_delta": {
        if (this.currentTool === undefined) throw new HermesCompletionError("Tool arguments have no active call");
        this.calls[this.currentTool].function.arguments += event.arguments;
        return this.chunk({ tool_calls: [{ index: this.currentTool, function: { arguments: event.arguments } }] });
      }
      case "tool_call_end": {
        if (this.currentTool === undefined) throw new HermesCompletionError("Tool completion has no active call");
        const call = this.calls[this.currentTool];
        let value: unknown;
        try { value = JSON.parse(call.function.arguments || "{}"); } catch { throw new HermesCompletionError("Model returned invalid tool JSON"); }
        if (!value || typeof value !== "object" || Array.isArray(value)) throw new HermesCompletionError("Model returned non-object tool arguments");
        if (!call.function.arguments) call.function.arguments = "{}";
        this.currentTool = undefined;
        return;
      }
      case "done":
      case "incomplete": {
        if (this.currentTool !== undefined) throw new HermesCompletionError("Model completed with unfinished tool arguments");
        if (!this.content && !this.calls.length && event.type === "done") throw new HermesCompletionError("Model completed without an answer or tool call");
        this.consumed = event.usage;
        this.terminal = event.type === "incomplete" ? "length" : this.calls.length ? "tool_calls" : "stop";
        return this.chunk({}, this.terminal);
      }
      default: return;
    }
  }

  json() {
    if (!this.terminal) throw new HermesCompletionError("Model stream ended without a terminal event");
    return { id: this.id, object: "chat.completion", created: this.created, model: this.model,
      choices: [{ index: 0, message: { role: "assistant", content: this.content || null,
        ...(this.reasoning ? { reasoning_content: this.reasoning } : {}), ...(this.calls.length ? { tool_calls: this.calls } : {}) }, finish_reason: this.terminal }],
      usage: usage(this.consumed) };
  }

  usageChunk() {
    this.json();
    return { id: this.id, object: "chat.completion.chunk", created: this.created, model: this.model, choices: [], usage: usage(this.consumed) };
  }
}

export function streamHermesCompletion(
  events: AsyncIterable<AdapterEvent>, completion: HermesCompletion, abort: () => void, includeUsage = false,
): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  const data = (value: unknown) => encoder.encode(`data: ${JSON.stringify(value)}\n\n`);
  async function* frames() {
    yield data(completion.chunk({ role: "assistant", content: "" }));
    try {
      for await (const event of events) {
        if (event.type === "heartbeat") { yield encoder.encode(": keep-alive\n\n"); continue; }
        const frame = completion.accept(event);
        if (frame) yield data(frame);
      }
      completion.json();
      if (includeUsage) yield data(completion.usageChunk());
      yield encoder.encode("data: [DONE]\n\n");
    } catch (error) {
      abort();
      yield data(completionError(error));
    }
  }
  const iterator = frames();
  return new ReadableStream({
    async pull(controller) {
      const next = await iterator.next();
      if (next.done) controller.close(); else controller.enqueue(next.value);
    },
    async cancel() { abort(); await iterator.return(undefined); },
  });
}
