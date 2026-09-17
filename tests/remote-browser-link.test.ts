import { expect, test } from "bun:test";
import {
  LAUNCHER_BROWSER_HOST_KIND,
  LAUNCHER_BROWSER_IDLE_URL,
} from "../src/launcher-browser-host";
import {
  buildLocalRemoteDescriptor,
  parseRemoteLauncherDescriptor,
} from "../src/remote-browser-link";

function remoteDescriptorJson(): string {
  const surfaceId = "s".repeat(32);
  return JSON.stringify({
    version: 3,
    kind: LAUNCHER_BROWSER_HOST_KIND,
    profile: "production",
    pid: 4242,
    endpoint: "http://127.0.0.1:30123",
    control: {
      endpoint: "http://127.0.0.1:30124",
      token: "launcher-control-token-0123456789abcdefghijklmnop",
    },
    helper: {
      executable: "/opt/codex-web-gpt/electron",
      script: "/opt/codex-web-gpt/browser-helper.cjs",
    },
    partition: "persist:codex-web-gpt-chatgpt",
    idleUrl: LAUNCHER_BROWSER_IDLE_URL,
    surfaceId,
    surfaceTargets: { [surfaceId]: "remote-native-target" },
    createdAt: "2026-09-18T00:00:00.000Z",
  });
}

test("remote browser link validates a launcher descriptor without requiring VPS helper paths locally", () => {
  const descriptor = parseRemoteLauncherDescriptor(remoteDescriptorJson());
  expect(descriptor).toMatchObject({
    profile: "production",
    pid: 4242,
    endpoint: "http://127.0.0.1:30123",
    control: { endpoint: "http://127.0.0.1:30124" },
  });
});

test("remote browser link builds a process-bound local descriptor using the local helper", () => {
  const remote = parseRemoteLauncherDescriptor(remoteDescriptorJson());
  const surfaceId = "f".repeat(32);
  const snapshot = {
    ...remote,
    surfaceId,
    surfaceTargets: { [surfaceId]: "fresh-native-target" },
    createdAt: "2026-09-18T00:01:00.000Z",
  };
  const descriptor = buildLocalRemoteDescriptor(
    remote,
    snapshot,
    "/local/runtime/browser-helper.cjs",
  );
  expect(descriptor).toMatchObject({
    version: 3,
    kind: LAUNCHER_BROWSER_HOST_KIND,
    profile: "production",
    pid: process.pid,
    remote: true,
    endpoint: "http://127.0.0.1:30123",
    control: {
      endpoint: "http://127.0.0.1:30124",
      token: remote.control.token,
    },
    helper: {
      executable: process.execPath,
      script: "/local/runtime/browser-helper.cjs",
    },
    surfaceId,
    surfaceTargets: { [surfaceId]: "fresh-native-target" },
    createdAt: "2026-09-18T00:01:00.000Z",
  });
});

test("remote browser link rejects endpoints that would expose CDP outside loopback", () => {
  const value = JSON.parse(remoteDescriptorJson());
  value.endpoint = "http://0.0.0.0:30123";
  expect(() => parseRemoteLauncherDescriptor(JSON.stringify(value)))
    .toThrow("http://127.0.0.1");
});
