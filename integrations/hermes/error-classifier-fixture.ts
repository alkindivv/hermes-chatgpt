import { defaultConfig } from "../../src/config";
import type { ProviderAdapter } from "../../src/adapters/base";
import { startHermesServer } from "../../src/hermes/server";

const apiToken = process.env.HERMES_CHATGPT_TEST_KEY;
if (!apiToken) throw new Error("HERMES_CHATGPT_TEST_KEY is required");

const adapter: ProviderAdapter = {
  name: "hermes-error-classifier-fixture",
  async runTurn(parsed, _incoming, emit) {
    const userText = parsed.context.messages
      .filter(message => message.role === "user" && typeof message.content === "string")
      .map(message => message.content)
      .join("\n");
    if (userText.includes("HERMES_AUXILIARY_CONTRACT")) {
      emit({ type: "text_delta", text: "HERMES_AUXILIARY_OK" });
      emit({ type: "done", stopReason: "stop" });
      return;
    }
    if (userText.includes("HERMES_CONTEXT_OVERFLOW_CONTRACT")) {
      emit({
        type: "error",
        message: "This prompt exceeds the measured ChatGPT browser message boundary.",
        status: 400,
        errorType: "invalid_request_error",
        code: "context_length_exceeded",
        retryable: false,
      });
      return;
    }
    if (userText.includes("HERMES_SUBMITTED_FAILURE_CONTRACT")) {
      emit({
        type: "error",
        message: "ChatGPT stopped responding after the task started. Check the ChatGPT tab before continuing.",
        status: 502,
        errorType: "server_error",
        code: "chatgpt_submitted_turn_failed",
        retryable: false,
      });
      return;
    }
    emit({ type: "text_delta", text: "partial" });
    emit({
      type: "error",
      message: "ChatGPT rate limit: too many requests. Try again in a few minutes.",
      status: 429,
      errorType: "rate_limit_error",
      code: "rate_limit_exceeded",
      retryable: true,
    });
  },
};

const runtime = {
  ...defaultConfig("full"),
  port: 0,
  appName: "Hermes ChatGPT",
  automaticAppName: "Hermes ChatGPT",
  solAvailable: true,
  extraHighAvailable: true,
  proAvailable: true,
  browserHost: "launcher",
  browserHostDescriptorPath: "/fixture/launcher-browser.json",
};
const backend = await startHermesServer(
  { runtime, namespace: "hermes-error-classifier-contract", apiToken },
  {
    adapterFactory: () => adapter,
    inspectBrowser: async () => ({ authenticated: true }),
  },
);

console.log(`HERMES_TEST_PORT=${backend.server.port}`);
await new Promise<void>(resolve => {
  process.once("SIGTERM", resolve);
  process.once("SIGINT", resolve);
});
await backend.close();
