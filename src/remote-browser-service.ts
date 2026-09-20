import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import {
  assertDurableRuntimeCommand,
  atomicWriteFile,
  currentRuntimeCommand,
  getConfigDir,
} from "./config";
import { processRunning, runCommand, runChecked } from "./process";
import {
  assertRemoteBrowserSshTarget,
  DEFAULT_REMOTE_BROWSER_DESCRIPTOR,
  remoteBrowserLocalDescriptorPath,
} from "./remote-browser-link";

const LABEL = "io.github.codex-chatgpt-web.remote-browser";

export interface RemoteBrowserServiceOptions {
  target: string;
  remoteDescriptorPath?: string;
  localDescriptorPath?: string;
  browserHelperScriptPath?: string;
  runtimeCommand?: string[];
}

export interface RemoteBrowserServiceStatus {
  supported: boolean;
  installed: boolean;
  loaded: boolean;
  running: boolean;
  label: string;
  definitionPath?: string;
}

function xml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function plistPath(): string {
  return join(homedir(), "Library", "LaunchAgents", `${LABEL}.plist`);
}

function launchDomain(): string {
  return `gui/${userInfo().uid}`;
}

function serviceTarget(): string {
  return `${launchDomain()}/${LABEL}`;
}

function assertMacOs(): void {
  if (process.platform !== "darwin") {
    throw new Error("Remote browser LaunchAgent is currently supported on macOS only");
  }
}

function normalizedOptions(options: RemoteBrowserServiceOptions) {
  const target = assertRemoteBrowserSshTarget(options.target);
  const remoteDescriptorPath = options.remoteDescriptorPath?.trim() || DEFAULT_REMOTE_BROWSER_DESCRIPTOR;
  if ((!remoteDescriptorPath.startsWith("/") && !remoteDescriptorPath.startsWith("~/"))
    || /[\r\n\0]/.test(remoteDescriptorPath)) {
    throw new Error("Remote descriptor path must be absolute or start with ~/");
  }
  const localDescriptorPath = remoteBrowserLocalDescriptorPath(options.localDescriptorPath);
  const browserHelperScriptPath = options.browserHelperScriptPath?.trim();
  if (browserHelperScriptPath && !isAbsolute(browserHelperScriptPath)) {
    throw new Error("Browser helper script path must be absolute");
  }
  const runtimeCommand = options.runtimeCommand ?? currentRuntimeCommand();
  assertDurableRuntimeCommand(runtimeCommand);
  return {
    target,
    remoteDescriptorPath,
    localDescriptorPath,
    ...(browserHelperScriptPath ? { browserHelperScriptPath: resolve(browserHelperScriptPath) } : {}),
    runtimeCommand,
  };
}

export function remoteBrowserServiceDefinition(options: RemoteBrowserServiceOptions): string {
  const resolved = normalizedOptions(options);
  const logDir = join(getConfigDir(), "logs");
  const args = [
    ...resolved.runtimeCommand,
    "remote-browser",
    "connect",
    resolved.target,
    "--remote-descriptor",
    resolved.remoteDescriptorPath,
    "--local-descriptor",
    resolved.localDescriptorPath,
    ...(resolved.browserHelperScriptPath
      ? ["--browser-helper-script", resolved.browserHelperScriptPath]
      : []),
  ];
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
${args.map(arg => `    <string>${xml(arg)}</string>`).join("\n")}
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>CODEX_CHATGPT_WEB_HOME</key>
    <string>${xml(getConfigDir())}</string>
    <key>HOME</key>
    <string>${xml(homedir())}</string>
    <key>PATH</key>
    <string>/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>${xml(join(logDir, "remote-browser.stdout.log"))}</string>
  <key>StandardErrorPath</key>
  <string>${xml(join(logDir, "remote-browser.stderr.log"))}</string>
  <key>ProcessType</key>
  <string>Background</string>
</dict>
</plist>
`;
}

export function getRemoteBrowserServiceStatus(): RemoteBrowserServiceStatus {
  if (process.platform !== "darwin") {
    return { supported: false, installed: false, loaded: false, running: false, label: LABEL };
  }
  const path = plistPath();
  const result = runCommand("launchctl", ["print", serviceTarget()]);
  return {
    supported: true,
    installed: existsSync(path),
    loaded: result.status === 0,
    running: result.status === 0 && /^\s*state = running\s*$/m.test(result.stdout),
    label: LABEL,
    definitionPath: path,
  };
}

async function waitForUnloaded(timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (getRemoteBrowserServiceStatus().loaded && Date.now() < deadline) {
    await new Promise(resolveWait => setTimeout(resolveWait, 50));
  }
  if (getRemoteBrowserServiceStatus().loaded) {
    throw new Error(`launchd did not unload ${LABEL} after ${timeoutMs}ms`);
  }
}

function activeForegroundLinkPid(path: string): number | undefined {
  if (!existsSync(path)) return undefined;
  try {
    const descriptor = JSON.parse(readFileSync(path, "utf8")) as { remote?: unknown; pid?: unknown };
    if (descriptor.remote !== true || !Number.isInteger(descriptor.pid) || (descriptor.pid as number) < 1) {
      return undefined;
    }
    return processRunning(descriptor.pid as number) ? descriptor.pid as number : undefined;
  } catch {
    return undefined;
  }
}

export function installRemoteBrowserService(
  options: RemoteBrowserServiceOptions,
): RemoteBrowserServiceStatus {
  assertMacOs();
  const current = getRemoteBrowserServiceStatus();
  const activePid = activeForegroundLinkPid(remoteBrowserLocalDescriptorPath(options.localDescriptorPath));
  if (activePid && !current.loaded) {
    throw new Error(
      `Foreground remote-browser link is still running (pid ${activePid}); stop it before installing the LaunchAgent`,
    );
  }
  const next = remoteBrowserServiceDefinition(options);
  if (current.loaded && (!current.installed || readFileSync(plistPath(), "utf8") !== next)) {
    throw new Error(
      "Refusing to replace a loaded remote-browser LaunchAgent; stop it before installing the update",
    );
  }
  mkdirSync(dirname(plistPath()), { recursive: true, mode: 0o700 });
  mkdirSync(join(getConfigDir(), "logs"), { recursive: true, mode: 0o700 });
  if (!current.installed || readFileSync(plistPath(), "utf8") !== next) {
    atomicWriteFile(plistPath(), next);
  }
  if (!current.loaded) runChecked("launchctl", ["bootstrap", launchDomain(), plistPath()]);
  return getRemoteBrowserServiceStatus();
}

export function startRemoteBrowserService(): RemoteBrowserServiceStatus {
  assertMacOs();
  if (!existsSync(plistPath())) {
    throw new Error("Remote browser LaunchAgent is not installed");
  }
  const status = getRemoteBrowserServiceStatus();
  if (!status.loaded) {
    runChecked("launchctl", ["bootstrap", launchDomain(), plistPath()]);
  } else if (!status.running) {
    runChecked("launchctl", ["kickstart", "-k", serviceTarget()]);
  }
  return getRemoteBrowserServiceStatus();
}

export async function stopRemoteBrowserService(): Promise<RemoteBrowserServiceStatus> {
  assertMacOs();
  if (getRemoteBrowserServiceStatus().loaded) {
    runChecked("launchctl", ["bootout", serviceTarget()]);
    await waitForUnloaded();
  }
  return getRemoteBrowserServiceStatus();
}

export async function restartRemoteBrowserService(): Promise<RemoteBrowserServiceStatus> {
  await stopRemoteBrowserService();
  return startRemoteBrowserService();
}

export async function uninstallRemoteBrowserService(): Promise<RemoteBrowserServiceStatus> {
  assertMacOs();
  await stopRemoteBrowserService();
  rmSync(plistPath(), { force: true });
  return getRemoteBrowserServiceStatus();
}
