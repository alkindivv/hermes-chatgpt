import { randomBytes, randomUUID } from "node:crypto";
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { atomicWriteFile, expandUserPath, parseConfig, type AppConfig } from "../config";

export const HERMES_CONNECTOR_NAME = "Hermes ChatGPT";
export interface HermesConfig { version: 1; namespace: string; apiKeyFile: string; runtime: AppConfig }

export function resolveHermesHome(explicit?: string): string {
  return resolve(expandUserPath(explicit || process.env.HERMES_CHATGPT_HOME || join(homedir(), ".hermes-chatgpt")));
}

function privateText(path: string): string {
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Expected a private regular file: ${path}`);
  if (process.platform !== "win32" && ((stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid()))) {
    throw new Error(`Unsafe owner or permissions on Hermes configuration: ${path}`);
  }
  return readFileSync(path, "utf8");
}

export function readHermesApiToken(path: string): string {
  const token = privateText(path).trim();
  if (!/^[A-Za-z0-9_-]{40,128}$/.test(token)) throw new Error("Invalid Hermes local API token file");
  return token;
}

function validate(home: string, value: unknown): HermesConfig {
  if (!isAbsolute(home)) throw new Error("Hermes bridge home must be absolute");
  if (existsSync(join(home, "config.json"))) throw new Error("Refusing to reuse a Codex/runtime home for Hermes; choose a separate --home");
  if (!value || typeof value !== "object") throw new Error("Invalid Hermes configuration");
  const data = value as Partial<HermesConfig>;
  if (data.version !== 1 || typeof data.namespace !== "string" || !/^[A-Za-z0-9_-]{20,128}$/.test(data.namespace)) throw new Error("Invalid Hermes configuration identity");
  if (data.apiKeyFile !== join(home, "secrets", "api.key")) throw new Error("Hermes API token must stay inside its own bridge home");
  const runtime = parseConfig(data.runtime, join(home, "hermes.json"));
  if (runtime.runtimeBackend !== "hermes" || runtime.mode !== "full" || runtime.browserHost !== "launcher"
    || runtime.browserInteractionMode !== "automatic" || runtime.appName !== HERMES_CONNECTOR_NAME
    || runtime.automaticAppName !== HERMES_CONNECTOR_NAME || !runtime.solAvailable) {
    throw new Error("Hermes requires an isolated full, automatic, Sol-capable launcher configuration");
  }
  if (runtime.brokerSocketPath !== join(home, "runtime", "hermes-broker.sock")) throw new Error("Hermes broker must stay inside its own bridge home");
  const tunnel = runtime.tunnel!;
  if (tunnel.profileDir !== join(home, "tunnel", "profiles") || !tunnel.alias.startsWith("hermes-") || !tunnel.profileName.startsWith("hermes-")) {
    throw new Error("Hermes tunnel must use its own profile directory and alias");
  }
  return { version: 1, namespace: data.namespace, apiKeyFile: data.apiKeyFile, runtime };
}

export function loadHermesConfig(home: string): HermesConfig {
  return validate(home, JSON.parse(privateText(join(home, "hermes.json"))));
}

export function saveHermesConfig(home: string, runtime: AppConfig): HermesConfig {
  const path = join(home, "hermes.json");
  const existing = existsSync(path) ? loadHermesConfig(home) : undefined;
  const config = validate(home, { version: 1, namespace: existing?.namespace ?? randomUUID(), apiKeyFile: join(home, "secrets", "api.key"), runtime });
  if (existsSync(config.apiKeyFile)) readHermesApiToken(config.apiKeyFile);
  else atomicWriteFile(config.apiKeyFile, `${randomBytes(48).toString("base64url")}\n`);
  atomicWriteFile(path, `${JSON.stringify(config, null, 2)}\n`);
  return config;
}
