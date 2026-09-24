import { timingSafeEqual } from "node:crypto";
import {
  inspectLauncherBrowserHost,
  inspectLauncherBrowserHostLiveness,
} from "../launcher-browser-host";
import { createChatGptWebAdapter } from "../adapters/chatgpt-web";
import { ChatGptWebAdapterError } from "../adapters/chatgpt-web/adapter-error";
import { closeChatGptBrowserWorkers } from "../adapters/chatgpt-web/browser-worker";
import { TurnBroker } from "../adapters/chatgpt-web/turn-broker";
import { chatGptTurnSessions } from "../adapters/chatgpt-web/turn-execution";
import type { ProviderAdapter } from "../adapters/base";
import { availableChatGptWebModelRoutes, CHATGPT_WEB_BACKEND_MODEL, resolveChatGptWebContextLimits } from "../chatgpt-web-models";
import { providerConfig, type AppConfig } from "../config";
import { AsyncEventQueue } from "../event-queue";
import { readJsonRequestBody } from "../http-body";
import { routeChatGptWebRequest } from "../server";
import type { AdapterEvent, CodexProviderConfig } from "../types";
import { VERSION } from "../version";
import { completionError, HermesCompletion, HermesCompletionError, streamHermesCompletion } from "./completion";
import { parseHermesRequest } from "./request";

export interface HermesServerConfig { runtime: AppConfig; namespace: string; apiToken: string }

function launcherBusyWithOwnedTurn(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /^Launcher ChatGPT session could not be verified: ChatGPT browser is running Codex turn [A-Za-z0-9_-]+$/.test(message);
}

export async function startHermesServer(config: HermesServerConfig, dependencies: {
  adapterFactory?: (provider: CodexProviderConfig) => ProviderAdapter;
  inspectBrowser?: (descriptorPath: string) => Promise<unknown>;
  inspectBrowserLiveness?: (descriptorPath: string) => Promise<unknown>;
} = {}) {
  const { runtime } = config;
  if (runtime.host !== "127.0.0.1") throw new Error("Hermes inference must bind to 127.0.0.1");
  if (runtime.appName !== "Hermes ChatGPT" || runtime.browserInteractionMode !== "automatic" || !runtime.solAvailable) {
    throw new Error("Hermes requires its dedicated connector and a Sol-capable automatic browser account");
  }
  if (!/^[A-Za-z0-9_-]{40,}$/.test(config.apiToken)) throw new Error("Hermes inference requires a strong API token");
  const expected = Buffer.from(`Bearer ${config.apiToken}`);
  const authorized = (request: Request) => {
    const actual = Buffer.from(request.headers.get("authorization") ?? "");
    return actual.length === expected.length && timingSafeEqual(actual, expected);
  };
  const broker = TurnBroker.forSocket(runtime.brokerSocketPath);
  await broker.listen();
  const factory = dependencies.adapterFactory ?? createChatGptWebAdapter;
  const inspectBrowser = dependencies.inspectBrowser ?? inspectLauncherBrowserHost;
  const inspectBrowserLiveness = dependencies.inspectBrowserLiveness
    ?? ((descriptorPath: string) => inspectLauncherBrowserHostLiveness(
      descriptorPath,
      { expectedProfile: "production" },
    ));
  const active = new Map<AbortController, Promise<void>>();
  // Hermes profiles intentionally use the stable effort aliases (chatgpt-web/high, etc.).
  // Codex 6.0 owns the new family-specific catalog, while the dedicated Hermes bridge keeps its
  // existing five-model API contract so deployed profile fallback order and capability caches
  // remain stable across the runtime update.
  const routes = availableChatGptWebModelRoutes(runtime, true)
    .filter(r => r.interactionMode === "automatic"
      && r.backendModel === CHATGPT_WEB_BACKEND_MODEL
      && r.legacy === true);
  let closing = false;
  let closePromise: Promise<void> | undefined;
  let server: ReturnType<typeof Bun.serve>;
  try {
    server = Bun.serve({
      hostname: "127.0.0.1", port: runtime.port, idleTimeout: 0, maxRequestBodySize: 64 * 1024 * 1024,
      async fetch(request) {
        const url = new URL(request.url);
        if (request.method === "GET" && url.pathname === "/healthz") return Response.json({ service: "hermes-chatgpt", version: VERSION, accepting_turns: !closing });
        if (!authorized(request)) return Response.json({ error: { type: "authentication_error", message: "Unauthorized" } }, { status: 401 });
        if (closing) return Response.json({ error: { message: "Hermes backend is shutting down" } }, { status: 503 });
        const modelCatalog = () => ({
          object: "list",
          data: routes.map(route => ({
            id: route.slug,
            object: "model",
            created: 0,
            owned_by: "hermes-chatgpt",
            context_window: route.interactionMode === "automatic"
              ? resolveChatGptWebContextLimits(route.backendModel, route.adapterEffort, runtime).contextWindow
              : undefined,
          })),
        });
        if (request.method === "GET" && url.pathname === "/v1/models") {
          return Response.json(modelCatalog());
        }
        if (request.method === "GET" && url.pathname === "/v1/models/verified") {
          // An active bridge-owned browser turn is stronger liveness evidence than a maintenance
          // probe: the launcher already authenticated and leased a ChatGPT surface to this runtime.
          // When idle, verify the authenticated launcher surface before claiming the provider is
          // healthy. This keeps model discovery/health honest without racing an active turn.
          if (chatGptTurnSessions.activeCount() === 0) {
            const descriptorPath = runtime.browserHostDescriptorPath;
            if (!descriptorPath) {
              return Response.json({
                error: {
                  type: "server_error",
                  code: "chatgpt_browser_unavailable",
                  message: "Hermes ChatGPT browser host descriptor is unavailable",
                  retryable: true,
                },
              }, { status: 503 });
            }
            try {
              await inspectBrowser(descriptorPath);
            } catch (error) {
              // A shared Electron host may already be serving an independent Codex turn.
              // That turn is positive authentication/liveness evidence; do not report the
              // provider unhealthy merely because the maintenance surface cannot be inspected.
              if (launcherBusyWithOwnedTurn(error)) {
                try {
                  await inspectBrowserLiveness(descriptorPath);
                } catch (livenessError) {
                  return Response.json({
                    error: {
                      type: "server_error",
                      code: "chatgpt_browser_unavailable",
                      message: livenessError instanceof Error
                        ? livenessError.message
                        : String(livenessError),
                      retryable: true,
                    },
                  }, { status: 503 });
                }
              } else {
                return Response.json({
                  error: {
                    type: "server_error",
                    code: "chatgpt_browser_unavailable",
                    message: error instanceof Error ? error.message : String(error),
                    retryable: true,
                  },
                }, { status: 503 });
              }
            }
          }
          return Response.json(modelCatalog());
        }
        if (request.method !== "POST" || url.pathname !== "/v1/chat/completions") return Response.json({ error: { message: "Not found" } }, { status: 404 });
        let raw: unknown;
        let parsed: ReturnType<typeof parseHermesRequest>;
        let model: string;
        try {
          raw = await readJsonRequestBody(request);
          parsed = parseHermesRequest(raw, config.namespace);
          model = String((raw as { model?: unknown }).model ?? parsed.modelId);
          if (!routes.some(r => r.slug === parsed.modelId)) {
            throw new Error(`Hermes model is not enabled: ${model}`);
          }
          routeChatGptWebRequest(parsed, runtime);
        } catch (error) {
          return Response.json({ error: { type: "invalid_request_error", message: error instanceof Error ? error.message : String(error) } }, { status: 400 });
        }
        const provider = providerConfig(runtime);
        // No Codex rollout/history files are consulted or written by the external Hermes executor.
        delete provider.chatgptWeb!.threadEnvironmentStatePath;
        delete provider.chatgptWeb!.lunaCheckpointStatePath;
        provider.chatgptWeb!.localToolsEnabled = Boolean((raw as { hermes?: { session_id?: string } }).hermes?.session_id);
        const queue = new AsyncEventQueue<AdapterEvent>();
        const controller = new AbortController();
        const abort = () => { controller.abort(); queue.close(); };
        if (request.signal.aborted) abort(); else request.signal.addEventListener("abort", abort, { once: true });
        let lastEvent = Date.now();
        const keepAlive = setInterval(() => {
          if (Date.now() - lastEvent < 10_000) return;
          try { queue.push({ type: "heartbeat" }); } catch { abort(); }
        }, 10_000);
        keepAlive.unref?.();
        const emit = (event: AdapterEvent) => { lastEvent = Date.now(); queue.push(event); };
        const run = Promise.resolve().then(async () => {
          if (!controller.signal.aborted) await factory(provider).runTurn(parsed, { headers: new Headers(), abortSignal: controller.signal }, emit);
        }).catch(error => {
          if (error instanceof ChatGptWebAdapterError) {
            emit({
              type: "error",
              message: error.message,
              status: error.status,
              errorType: error.errorType,
              code: error.code,
              retryable: error.retryable,
            });
          } else {
            emit({ type: "error", message: error instanceof Error ? error.message : String(error) });
          }
        }).finally(() => {
          clearInterval(keepAlive);
          queue.close();
          request.signal.removeEventListener("abort", abort);
          active.delete(controller);
        });
        active.set(controller, run);
        const completion = new HermesCompletion(model, parsed.options.parallelToolCalls !== false);
        if (parsed.stream) {
          const includeUsage = (raw as { stream_options?: { include_usage?: boolean } }).stream_options?.include_usage === true;
          return new Response(streamHermesCompletion(queue, completion, abort, includeUsage), {
            headers: { "content-type": "text/event-stream", "cache-control": "no-cache", "x-accel-buffering": "no" },
          });
        }
        try {
          for await (const event of queue) completion.accept(event);
          return Response.json(completion.json());
        } catch (error) {
          abort();
          const status = error instanceof HermesCompletionError ? error.status : 502;
          return Response.json(completionError(error), { status: status >= 400 && status <= 599 ? status : 502 });
        }
      },
    });
  } catch (error) { await broker.close(); throw error; }
  return {
    server,
    close(): Promise<void> {
      if (closePromise) return closePromise;
      closing = true;
      closePromise = (async () => {
        for (const controller of active.keys()) controller.abort();
        chatGptTurnSessions.clear();
        await Promise.all(active.values());
        await closeChatGptBrowserWorkers();
        await broker.close();
        server.stop(true);
      })();
      return closePromise;
    },
  };
}
