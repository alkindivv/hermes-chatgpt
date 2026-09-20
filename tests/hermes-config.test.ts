import { afterEach, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { defaultConfig } from "../src/config";
import { mcpCommand } from "../src/tunnel";
import { loadHermesConfig, saveHermesConfig, readHermesApiToken } from "../src/hermes/config";
import { resolveHermesRuntimeKeyInput } from "../src/hermes/cli";
import { chatGptConnectorMentionQuery } from "../src/adapters/chatgpt-web/browser-worker";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });
function fixture() {
  const home = mkdtempSync(join(tmpdir(), "hermes-config-")); roots.push(home);
  const runtime = { ...defaultConfig("full"), runtimeBackend: "hermes" as const, appName: "Hermes ChatGPT", automaticAppName: "Hermes ChatGPT", browserHost: "launcher" as const,
    browserHostDescriptorPath: join(home, "browser.json"), brokerSocketPath: join(home, "runtime", "hermes-broker.sock"),
    runtimeCommand: [process.execPath],
    tunnel: { binaryPath: process.execPath, tunnelId: "tunnel_" + "a".repeat(32), runtimeKeyFile: join(home, "secrets", "tunnel.key"), profileDir: join(home, "tunnel", "profiles"), profileName: "hermes-test", alias: "hermes-test" } };
  return { home, runtime };
}

test("Hermes config and local auth key are isolated, private, and preserve their identity on updates", () => {
  const { home, runtime } = fixture();
  const first = saveHermesConfig(home, runtime);
  const originalKey = readHermesApiToken(first.apiKeyFile);
  expect(originalKey.length).toBeGreaterThanOrEqual(40);
  expect(existsSync(join(home, "config.json"))).toBe(false);
  expect(loadHermesConfig(home).runtime.appName).toBe("Hermes ChatGPT");
  const second = saveHermesConfig(home, { ...runtime, port: 17843 });
  expect(second.namespace).toBe(first.namespace);
  expect(readHermesApiToken(second.apiKeyFile)).toBe(originalKey);
  expect(statSync(second.apiKeyFile).mode & 0o077).toBe(0);
  expect(statSync(join(home, "hermes.json")).mode & 0o077).toBe(0);
  chmodSync(second.apiKeyFile, 0o644);
  expect(() => readHermesApiToken(second.apiKeyFile)).toThrow("permissions");
});

test("setup refuses an existing Codex home or a foreign broker/tunnel path", () => {
  const { home, runtime } = fixture();
  expect(() => saveHermesConfig(home, { ...runtime, brokerSocketPath: "/tmp/mac.sock" })).toThrow("broker");
  expect(() => saveHermesConfig(home, { ...runtime, tunnel: { ...runtime.tunnel, alias: "codex-chatgpt-web" } })).toThrow("tunnel");
  writeFileSync(join(home, "config.json"), "ORIGINAL");
  expect(() => saveHermesConfig(home, runtime)).toThrow("Codex");
  expect(readFileSync(join(home, "config.json"), "utf8")).toBe("ORIGINAL");
});

test("Hermes runtime key input prefers one explicit source and fails closed on missing env", () => {
  expect(resolveHermesRuntimeKeyInput({ fileArg: "/tmp/key", envArg: undefined, existingPath: undefined, env: {} }))
    .toEqual({ type: "file", value: "/tmp/key" });
  expect(resolveHermesRuntimeKeyInput({ fileArg: undefined, envArg: "OPENAI_API_TUNNEL", existingPath: undefined, env: { OPENAI_API_TUNNEL: " secret-value " } }))
    .toEqual({ type: "value", value: "secret-value" });
  expect(() => resolveHermesRuntimeKeyInput({ fileArg: "/tmp/key", envArg: "OPENAI_API_TUNNEL", existingPath: undefined, env: { OPENAI_API_TUNNEL: "x" } }))
    .toThrow("Choose only one");
  expect(() => resolveHermesRuntimeKeyInput({ fileArg: undefined, envArg: "OPENAI_API_TUNNEL", existingPath: undefined, env: {} }))
    .toThrow("missing or empty");
  expect(resolveHermesRuntimeKeyInput({ fileArg: undefined, envArg: undefined, existingPath: "/managed/key", env: {} }))
    .toEqual({ type: "file", value: "/managed/key" });
});

test("only a Hermes MCP command opts in to the Hermes backend", () => {
  const { runtime } = fixture();
  expect(mcpCommand(runtime)).toContain("--backend hermes");
  expect(mcpCommand({ ...runtime, runtimeBackend: undefined })).not.toContain("--backend");
});

test("connector discovery follows configured identity without changing Codex's query", () => {
  expect(chatGptConnectorMentionQuery("Codex Native2")).toBe("@codex");
  expect(chatGptConnectorMentionQuery("Hermes ChatGPT")).toBe("@hermes");
  expect(() => chatGptConnectorMentionQuery("@invalid")).toThrow();
});
