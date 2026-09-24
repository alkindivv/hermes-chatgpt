import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { stdout } from "node:process";
import { expandUserPath, getConfigDir } from "./config";
import {
  LAUNCHER_BROWSER_HOST_KIND,
  LAUNCHER_BROWSER_IDLE_URL,
  type LauncherBrowserHostDescriptor,
} from "./launcher-browser-host";

export const DEFAULT_REMOTE_BROWSER_DESCRIPTOR = "~/.codex-chatgpt-web/runtime/launcher-browser.json";
const CONNECT_TIMEOUT_MS = 15_000;
const SSH_CONNECT_TIMEOUT_SECONDS = 15;

export function remoteBrowserSshNonInteractiveArgs(): string[] {
  return [
    "-o", "BatchMode=yes",
    "-o", `ConnectTimeout=${SSH_CONNECT_TIMEOUT_SECONDS}`,
  ];
}

export interface RemoteBrowserLinkOptions {
  target: string;
  remoteDescriptorPath?: string;
  localDescriptorPath?: string;
  browserHelperScriptPath?: string;
  sshExecutable?: string;
}

interface RemoteLauncherDescriptor {
  version: 3;
  kind: typeof LAUNCHER_BROWSER_HOST_KIND;
  profile: "production" | "development";
  pid: number;
  endpoint: string;
  control: {
    endpoint: string;
    token: string;
  };
  helper: {
    executable: string;
    script: string;
  };
  partition: string;
  idleUrl: string;
  surfaceId: string;
  surfaceTargets: Record<string, string>;
  createdAt: string;
}

export function assertRemoteBrowserSshTarget(value: string): string {
  const target = value.trim();
  if (!target || target.startsWith("-") || !/^[A-Za-z0-9_.@%:+-]+$/.test(target)) {
    throw new Error("SSH target must be a plain host or user@host value");
  }
  return target;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function remoteDescriptorReadCommand(path: string): string {
  if (path.startsWith("~/")) {
    return `cat -- "$HOME"/${shellQuote(path.slice(2))}`;
  }
  if (!path.startsWith("/")) {
    throw new Error("Remote descriptor path must be absolute or start with ~/");
  }
  return `cat -- ${shellQuote(path)}`;
}

function remoteDescriptorOwnerCommand(path: string): string {
  const resolved = path.startsWith("~/")
    ? `"$HOME"/${shellQuote(path.slice(2))}`
    : path.startsWith("/")
      ? shellQuote(path)
      : (() => { throw new Error("Remote descriptor path must be absolute or start with ~/"); })();
  return `owner="$(stat -c %U -- ${resolved} 2>/dev/null || stat -f %Su -- ${resolved} 2>/dev/null)"`
    + ` && printf "%s\\n" "$owner"`;
}

function loopbackPort(value: unknown, label: string): number {
  if (typeof value !== "string") throw new Error(`${label} is missing`);
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new Error(`${label} is invalid`);
  }
  if (parsed.protocol !== "http:" || parsed.hostname !== "127.0.0.1"
    || !parsed.port || parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new Error(`${label} must use http://127.0.0.1 with an explicit port`);
  }
  return Number(parsed.port);
}

function assertSurfaceTargets(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.entries(value).some(([surface, target]) => !/^[A-Za-z0-9_-]{32}$/.test(surface)
      || typeof target !== "string" || !target.trim())
    || new Set(Object.values(value)).size !== Object.keys(value).length) {
    throw new Error("Remote launcher descriptor has invalid or duplicated surface targets");
  }
  return value as Record<string, string>;
}

export function parseRemoteLauncherDescriptor(text: string): RemoteLauncherDescriptor {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new Error(`Remote launcher descriptor is invalid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Remote launcher descriptor is not an object");
  }
  const descriptor = value as Partial<RemoteLauncherDescriptor>;
  if (descriptor.version !== 3 || descriptor.kind !== LAUNCHER_BROWSER_HOST_KIND) {
    throw new Error("Remote launcher descriptor has an unsupported identity or version");
  }
  if (descriptor.profile !== "production" && descriptor.profile !== "development") {
    throw new Error("Remote launcher descriptor has an invalid profile");
  }
  if (!Number.isInteger(descriptor.pid) || descriptor.pid! < 1) {
    throw new Error("Remote launcher descriptor has an invalid pid");
  }
  loopbackPort(descriptor.endpoint, "Remote launcher CDP endpoint");
  if (!descriptor.control || typeof descriptor.control !== "object") {
    throw new Error("Remote launcher descriptor is missing its control channel");
  }
  loopbackPort(descriptor.control.endpoint, "Remote launcher control endpoint");
  if (typeof descriptor.control.token !== "string"
    || !/^[A-Za-z0-9_-]{40,}$/.test(descriptor.control.token)) {
    throw new Error("Remote launcher descriptor has an invalid control token");
  }
  if (!descriptor.helper || typeof descriptor.helper.executable !== "string"
    || typeof descriptor.helper.script !== "string") {
    throw new Error("Remote launcher descriptor is missing helper metadata");
  }
  const expectedPartition = descriptor.profile === "development"
    ? "persist:codex-web-gpt-dev-chatgpt"
    : "persist:codex-web-gpt-chatgpt";
  if (descriptor.partition !== expectedPartition) {
    throw new Error("Remote launcher descriptor identifies an unexpected browser partition");
  }
  if (descriptor.idleUrl !== LAUNCHER_BROWSER_IDLE_URL) {
    throw new Error("Remote launcher descriptor identifies an unexpected idle surface");
  }
  if (typeof descriptor.surfaceId !== "string" || !/^[A-Za-z0-9_-]{32}$/.test(descriptor.surfaceId)) {
    throw new Error("Remote launcher descriptor has an invalid owned surface id");
  }
  const surfaceTargets = assertSurfaceTargets(descriptor.surfaceTargets);
  if (typeof descriptor.createdAt !== "string" || Number.isNaN(Date.parse(descriptor.createdAt))) {
    throw new Error("Remote launcher descriptor has an invalid creation time");
  }
  return {
    version: 3,
    kind: LAUNCHER_BROWSER_HOST_KIND,
    profile: descriptor.profile,
    pid: descriptor.pid!,
    endpoint: descriptor.endpoint!,
    control: {
      endpoint: descriptor.control.endpoint,
      token: descriptor.control.token,
    },
    helper: {
      executable: descriptor.helper.executable,
      script: descriptor.helper.script,
    },
    partition: descriptor.partition,
    idleUrl: descriptor.idleUrl,
    surfaceId: descriptor.surfaceId,
    surfaceTargets,
    createdAt: descriptor.createdAt,
  };
}

function browserHelperScript(configured?: string): string {
  if (configured) {
    const explicit = resolve(expandUserPath(configured));
    if (!existsSync(explicit)) throw new Error(`Browser helper does not exist: ${explicit}`);
    return explicit;
  }
  const entrypoint = process.argv[1];
  if (typeof entrypoint === "string" && basename(entrypoint) === "cli.js") {
    const sibling = join(dirname(entrypoint), "browser-helper.cjs");
    if (existsSync(sibling)) return sibling;
  }
  const sourceHelper = resolve(import.meta.dir, "adapters", "chatgpt-web", "browser-helper-main.ts");
  if (existsSync(sourceHelper)) return sourceHelper;
  throw new Error(
    "Could not locate the local browser helper; use --browser-helper-script with an absolute path",
  );
}

function privateWrite(path: string, body: string): void {
  const directory = dirname(path);
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  if (process.platform !== "win32") chmodSync(directory, 0o700);
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, body, { mode: 0o600 });
  renameSync(temporary, path);
  if (process.platform !== "win32") chmodSync(path, 0o600);
}

export function remoteBrowserLocalDescriptorPath(configured?: string): string {
  if (!configured) return join(getConfigDir(), "runtime", "remote-launcher-browser.json");
  const expanded = expandUserPath(configured);
  if (!isAbsolute(expanded)) throw new Error("--local-descriptor must be an absolute path");
  return resolve(expanded);
}

function fetchRemoteDescriptor(
  sshExecutable: string,
  target: string,
  remoteDescriptorPath: string,
): RemoteLauncherDescriptor {
  const command = remoteDescriptorReadCommand(remoteDescriptorPath);
  const result = spawnSync(
    sshExecutable,
    [...remoteBrowserSshNonInteractiveArgs(), target, command],
    {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: CONNECT_TIMEOUT_MS + 5_000,
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = result.stderr.trim();
    throw new Error(
      `Could not read launcher descriptor from ${target}: ${detail || `ssh exited ${result.status ?? 1}`}`,
    );
  }
  return parseRemoteLauncherDescriptor(result.stdout);
}

function fetchRemoteDescriptorOwner(
  sshExecutable: string,
  target: string,
  remoteDescriptorPath: string,
): string {
  const result = spawnSync(
    sshExecutable,
    [
      ...remoteBrowserSshNonInteractiveArgs(),
      target,
      remoteDescriptorOwnerCommand(remoteDescriptorPath),
    ],
    {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: CONNECT_TIMEOUT_MS + 5_000,
    },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = result.stderr.trim();
    throw new Error(
      `Could not determine launcher descriptor owner on ${target}: ${detail || `ssh exited ${result.status ?? 1}`}`,
    );
  }
  const owner = result.stdout.trim();
  if (!/^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(owner)) {
    throw new Error("Remote launcher descriptor owner is invalid");
  }
  return owner;
}

function snapshotMatches(
  body: unknown,
  descriptor: RemoteLauncherDescriptor,
): body is RemoteLauncherDescriptor {
  if (!body || typeof body !== "object" || Array.isArray(body)) return false;
  const snapshot = body as Partial<RemoteLauncherDescriptor>;
  try {
    return snapshot.version === 3
      && snapshot.kind === LAUNCHER_BROWSER_HOST_KIND
      && snapshot.profile === descriptor.profile
      && snapshot.partition === descriptor.partition
      && snapshot.idleUrl === descriptor.idleUrl
      && typeof snapshot.surfaceId === "string"
      && /^[A-Za-z0-9_-]{32}$/.test(snapshot.surfaceId)
      && Boolean(assertSurfaceTargets(snapshot.surfaceTargets))
      && typeof snapshot.createdAt === "string"
      && !Number.isNaN(Date.parse(snapshot.createdAt));
  } catch {
    return false;
  }
}

async function waitForTunnel(
  child: ChildProcess,
  descriptor: RemoteLauncherDescriptor,
  timeoutMs = CONNECT_TIMEOUT_MS,
): Promise<RemoteLauncherDescriptor> {
  const deadline = Date.now() + timeoutMs;
  let lastError = "tunnel is not ready";
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`SSH tunnel exited before becoming ready (${child.signalCode || child.exitCode})`);
    }
    try {
      const cdp = await fetch(`${descriptor.endpoint}/json/version`);
      if (!cdp.ok) throw new Error(`CDP HTTP ${cdp.status}`);
      const metadata = await cdp.json() as Record<string, unknown>;
      const cdpPort = loopbackPort(descriptor.endpoint, "Remote launcher CDP endpoint");
      if (typeof metadata.webSocketDebuggerUrl !== "string"
        || !metadata.webSocketDebuggerUrl.startsWith(`ws://127.0.0.1:${cdpPort}/`)) {
        throw new Error("CDP WebSocket metadata does not match the forwarded port");
      }
      const snapshotResponse = await fetch(`${descriptor.control.endpoint}/v1/browser/descriptor`, {
        method: "POST",
        headers: {
          authorization: `Bearer ${descriptor.control.token}`,
          "content-type": "application/json",
        },
        body: "{}",
      });
      const snapshot = await snapshotResponse.json().catch(() => ({}));
      if (!snapshotResponse.ok) throw new Error(`control HTTP ${snapshotResponse.status}`);
      if (!snapshotMatches(snapshot, descriptor)) {
        throw new Error("control channel returned an incompatible descriptor snapshot");
      }
      return snapshot;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
      await Bun.sleep(100);
    }
  }
  throw new Error(`SSH tunnel did not become ready: ${lastError}`);
}

function cleanupDescriptor(path: string): void {
  try {
    const current = JSON.parse(readFileSync(path, "utf8")) as {
      pid?: unknown;
      remote?: unknown;
    };
    if (current.remote === true && current.pid === process.pid) {
      rmSync(path, { force: true });
    }
  } catch {}
}

export function buildLocalRemoteDescriptor(
  remote: RemoteLauncherDescriptor,
  snapshot: RemoteLauncherDescriptor,
  helperScript: string,
  remoteHelper?: {
    sshExecutable: string;
    target: string;
    descriptorPath: string;
    owner: string;
  },
): LauncherBrowserHostDescriptor {
  return {
    version: 3,
    kind: LAUNCHER_BROWSER_HOST_KIND,
    profile: remote.profile,
    pid: process.pid,
    remote: true,
    endpoint: remote.endpoint,
    control: remote.control,
    helper: {
      executable: process.execPath,
      script: helperScript,
      ...(remoteHelper ? {
        remote: {
          sshExecutable: remoteHelper.sshExecutable,
          target: remoteHelper.target,
          descriptorPath: remoteHelper.descriptorPath,
          owner: remoteHelper.owner,
          executable: remote.helper.executable,
          script: remote.helper.script,
        },
      } : {}),
    },
    partition: remote.partition,
    idleUrl: remote.idleUrl,
    surfaceId: snapshot.surfaceId,
    surfaceTargets: snapshot.surfaceTargets,
    createdAt: snapshot.createdAt,
  };
}

export async function connectRemoteBrowserLink(options: RemoteBrowserLinkOptions): Promise<void> {
  const target = assertRemoteBrowserSshTarget(options.target);
  const sshExecutable = options.sshExecutable?.trim() || "ssh";
  const remotePath = options.remoteDescriptorPath?.trim() || DEFAULT_REMOTE_BROWSER_DESCRIPTOR;
  if (!remotePath || /[\r\n\0]/.test(remotePath)) {
    throw new Error("Remote descriptor path is invalid");
  }
  const destination = remoteBrowserLocalDescriptorPath(options.localDescriptorPath);
  const helperScript = browserHelperScript(options.browserHelperScriptPath);
  const remote = fetchRemoteDescriptor(sshExecutable, target, remotePath);
  const remoteOwner = fetchRemoteDescriptorOwner(sshExecutable, target, remotePath);
  const cdpPort = loopbackPort(remote.endpoint, "Remote launcher CDP endpoint");
  const controlPort = loopbackPort(remote.control.endpoint, "Remote launcher control endpoint");
  if (cdpPort === controlPort) throw new Error("Remote launcher reused one port for CDP and control");

  const tunnel = spawn(sshExecutable, [
    "-N",
    "-T",
    ...remoteBrowserSshNonInteractiveArgs(),
    "-o", "ExitOnForwardFailure=yes",
    "-o", "ServerAliveInterval=30",
    "-o", "ServerAliveCountMax=3",
    "-L", `127.0.0.1:${cdpPort}:127.0.0.1:${cdpPort}`,
    "-L", `127.0.0.1:${controlPort}:127.0.0.1:${controlPort}`,
    target,
  ], {
    stdio: ["ignore", "inherit", "inherit"],
  });

  const stop = () => {
    if (tunnel.exitCode === null && tunnel.signalCode === null) tunnel.kill("SIGTERM");
  };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);

  try {
    let rejectStartupError!: (error: Error) => void;
    const startupError = new Promise<never>((_resolve, reject) => {
      rejectStartupError = reject;
    });
    const onStartupError = (error: Error) => rejectStartupError(error);
    tunnel.once("error", onStartupError);
    let snapshot: RemoteLauncherDescriptor;
    try {
      snapshot = await Promise.race([
        waitForTunnel(tunnel, remote),
        startupError,
      ]);
    } finally {
      tunnel.off("error", onStartupError);
    }
    const local = buildLocalRemoteDescriptor(remote, snapshot, helperScript, {
      sshExecutable,
      target,
      descriptorPath: remotePath,
      owner: remoteOwner,
    });
    privateWrite(destination, `${JSON.stringify(local, null, 2)}\n`);
    stdout.write(
      `Remote browser link ready.\nDescriptor: ${destination}\n`
      + `CDP: 127.0.0.1:${cdpPort} -> ${target}\n`
      + `Control: 127.0.0.1:${controlPort} -> ${target}\n`
      + `Browser helper: SSH -> ${target} (runs as ${remoteOwner})\n`
      + "Keep this process running while Codex uses the remote browser.\n",
    );

    const result = await new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveExit, rejectExit) => {
      tunnel.once("error", rejectExit);
      tunnel.once("exit", (code, signal) => resolveExit({ code, signal }));
    });
    if (result.code !== 0 && result.signal !== "SIGTERM" && result.signal !== "SIGINT") {
      throw new Error(`SSH tunnel exited unexpectedly (${result.signal || (result.code ?? 1)})`);
    }
  } finally {
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
    cleanupDescriptor(destination);
    stop();
  }
}
