import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { currentRuntimeCommand, defaultConfig, expandUserPath } from "../config";
import { inspectLauncherBrowserHost, inspectLauncherBrowserHostLiveness, readLauncherBrowserHostDescriptor } from "../launcher-browser-host";
import { connectTunnel, createTunnelConfig, installRuntimeKey, installTunnelClient, stopTunnel, tunnelStatus, waitForTunnelReady } from "../tunnel";
import { HERMES_CONNECTOR_NAME, loadHermesConfig, readHermesApiToken, resolveHermesHome, saveHermesConfig } from "./config";
import { startHermesServer } from "./server";

function option(args: string[], name: string): string | undefined {
  const index = args.indexOf(name);
  if (index < 0) return;
  const value = args[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`${name} requires a value`);
  args.splice(index, 2);
  return value;
}
function noArgs(args: string[]) { if (args.length) throw new Error(`Unknown Hermes arguments: ${args.join(" ")}`); }

async function setup(home: string, args: string[]): Promise<void> {
  if (existsSync(join(home, "config.json"))) throw new Error("Choose a separate Hermes bridge home, not the existing Codex home");
  const descriptorArg = option(args, "--browser-host-descriptor");
  const tunnelArg = option(args, "--tunnel-id");
  const keyArg = option(args, "--runtime-key-file");
  const portArg = option(args, "--port");
  const acknowledged = args.includes("--acknowledge-unofficial");
  if (acknowledged) args.splice(args.indexOf("--acknowledge-unofficial"), 1);
  noArgs(args);
  if (!acknowledged) throw new Error("Hermes uses unofficial browser automation. Pass --acknowledge-unofficial after reviewing its limitations");
  const existing = existsSync(join(home, "hermes.json")) ? loadHermesConfig(home) : undefined;
  const descriptorPath = descriptorArg ? resolve(expandUserPath(descriptorArg)) : existing?.runtime.browserHostDescriptorPath;
  const tunnelId = tunnelArg ?? existing?.runtime.tunnel?.tunnelId;
  if (!descriptorPath || !tunnelId) throw new Error("Hermes setup needs --browser-host-descriptor and a dedicated --tunnel-id (not the Mac tunnel)");
  if (!keyArg && !existing?.runtime.tunnel?.runtimeKeyFile) throw new Error("Hermes setup needs --runtime-key-file");
  const port = portArg === undefined ? existing?.runtime.port ?? 17842 : Number(portArg);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("--port must be an integer from 1 to 65535");
  const descriptor = readLauncherBrowserHostDescriptor(descriptorPath);
  if (descriptor.profile !== "production") throw new Error("Use the production Electron browser descriptor, not the DEV launcher profile");
  const capability = await inspectLauncherBrowserHost(descriptorPath, { detectCapabilities: true, expectedProfile: "production" });
  if (!capability.solAvailable) throw new Error("This Hermes backend currently requires a Sol-capable account; Luna rolling checkpoints remain Codex-specific");
  const runtimeKeyFile = keyArg ? installRuntimeKey(resolve(expandUserPath(keyArg))) : existing!.runtime.tunnel!.runtimeKeyFile;
  const binaryPath = await installTunnelClient();
  const alias = `hermes-${createHash("sha256").update(home).digest("hex").slice(0, 12)}`;
  const tunnel = createTunnelConfig({ binaryPath, tunnelId, runtimeKeyFile, profileName: alias, alias });
  const config = saveHermesConfig(home, {
    ...defaultConfig("full"), runtimeBackend: "hermes", port,
    appName: HERMES_CONNECTOR_NAME, automaticAppName: HERMES_CONNECTOR_NAME,
    browserHost: "launcher", browserHostRemote: descriptor.remote === true, browserInteractionMode: "automatic",
    browserHostDescriptorPath: descriptorPath, brokerSocketPath: join(home, "runtime", "hermes-broker.sock"),
    runtimeCommand: [...currentRuntimeCommand(), "--home", home],
    solAvailable: true, extraHighAvailable: capability.extraHighAvailable === true, proAvailable: capability.proAvailable === true,
    tunnel, automaticTunnel: tunnel, acknowledgedUnofficialAt: new Date().toISOString(),
  });
  console.log(`Hermes bridge configured: ${join(home, "hermes.json")}\nLocal API token file: ${config.apiKeyFile}\nConnector: ${HERMES_CONNECTOR_NAME}\nNext: hermes serve. Create the separate ChatGPT connector against this Hermes tunnel. No Codex route or Hermes profile was modified.`);
}

async function serve(home: string): Promise<void> {
  const config = loadHermesConfig(home);
  await inspectLauncherBrowserHostLiveness(config.runtime.browserHostDescriptorPath!, { expectedProfile: "production" });
  const apiToken = readHermesApiToken(config.apiKeyFile);
  const host = await startHermesServer({ runtime: config.runtime, namespace: config.namespace, apiToken });
  let stop!: () => void;
  const stopped = new Promise<void>(resolveStop => { stop = resolveStop; });
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  try {
    connectTunnel(config.runtime);
    const ready = await waitForTunnelReady(config.runtime);
    if (!ready.ok) throw new Error(`Hermes tunnel is not ready: ${ready.detail}`);
    console.log(`Hermes ChatGPT listening on http://127.0.0.1:${host.server.port}/v1\nTools execute in Hermes. Keep the Electron host running.`);
    await stopped;
  } finally {
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
    const results = await Promise.allSettled([host.close(), Promise.resolve().then(() => stopTunnel(config.runtime))]);
    const errors = results.flatMap(r => r.status === "rejected" ? [r.reason] : []);
    if (errors.length) throw new AggregateError(errors, "Hermes shutdown did not fully complete");
  }
}

export async function runHermesCommand(args: string[], explicitHome?: string): Promise<void> {
  const home = resolveHermesHome(explicitHome);
  const before = process.env.CODEX_CHATGPT_WEB_HOME;
  process.env.CODEX_CHATGPT_WEB_HOME = home;
  try {
    const action = args.shift() ?? "status";
    if (action === "setup") { await setup(home, args); return; }
    if (action === "serve") { noArgs(args); await serve(home); return; }
    if (action === "tunnel") {
      const operation = args.shift() ?? "status";
      noArgs(args);
      const config = loadHermesConfig(home);
      if (operation === "start") { connectTunnel(config.runtime); console.log(JSON.stringify(await waitForTunnelReady(config.runtime), null, 2)); }
      else if (operation === "stop") { stopTunnel(config.runtime); console.log("Hermes tunnel stopped."); }
      else if (operation === "status") console.log(JSON.stringify(tunnelStatus(config.runtime), null, 2));
      else throw new Error("Hermes tunnel action must be start, stop or status");
      return;
    }
    if (action !== "status") throw new Error("Hermes command must be setup, serve, status or tunnel");
    noArgs(args);
    const config = loadHermesConfig(home);
    let health: unknown;
    try {
      const response = await fetch(`http://127.0.0.1:${config.runtime.port}/v1/models`, { headers: { authorization: `Bearer ${readHermesApiToken(config.apiKeyFile)}` }, signal: AbortSignal.timeout(5_000) });
      health = response.ok ? { ready: true, models: (await response.json() as { data: unknown }).data } : { ready: false, status: response.status };
    } catch { health = { ready: false }; }
    console.log(JSON.stringify({ home, connector: HERMES_CONNECTOR_NAME, port: config.runtime.port, health }, null, 2));
  } finally {
    if (before === undefined) delete process.env.CODEX_CHATGPT_WEB_HOME;
    else process.env.CODEX_CHATGPT_WEB_HOME = before;
  }
}
