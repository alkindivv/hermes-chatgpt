import { afterEach, expect, test } from "bun:test";
import { remoteBrowserServiceDefinition } from "../src/remote-browser-service";

const previousHome = process.env.CODEX_CHATGPT_WEB_HOME;

afterEach(() => {
  if (previousHome === undefined) delete process.env.CODEX_CHATGPT_WEB_HOME;
  else process.env.CODEX_CHATGPT_WEB_HOME = previousHome;
});

test("remote browser LaunchAgent persists the durable foreground connect command", () => {
  process.env.CODEX_CHATGPT_WEB_HOME = "/Users/test/.codex-chatgpt-web";
  const definition = remoteBrowserServiceDefinition({
    target: "root@54.37.252.127",
    remoteDescriptorPath: "/home/ubuntu/.codex-chatgpt-web/runtime/launcher-browser.json",
    localDescriptorPath: "/Users/test/.codex-chatgpt-web/runtime/remote-launcher-browser.json",
    runtimeCommand: [process.execPath, "/opt/codex-chatgpt-web/app/cli.js"],
  });

  expect(definition).toContain("<string>remote-browser</string>");
  expect(definition).toContain("<string>connect</string>");
  expect(definition).toContain("<string>root@54.37.252.127</string>");
  expect(definition).toContain(
    "<string>/home/ubuntu/.codex-chatgpt-web/runtime/launcher-browser.json</string>",
  );
  expect(definition).toContain(
    "<string>/Users/test/.codex-chatgpt-web/runtime/remote-launcher-browser.json</string>",
  );
  expect(definition).toContain("<key>RunAtLoad</key>\n  <true/>");
  expect(definition).toContain("<key>KeepAlive</key>\n  <true/>");
  expect(definition).toContain("<key>ThrottleInterval</key>\n  <integer>10</integer>");
  expect(definition).toContain("remote-browser.stdout.log");
  expect(definition).toContain("remote-browser.stderr.log");
  expect(definition).toContain("<key>PATH</key>\n    <string>/usr/bin:/bin:/usr/sbin:/sbin</string>");
  expect(definition).not.toContain("launcher-control-token");
  expect(definition).not.toContain("OPENAI_API_TUNNEL");
  expect(definition).not.toContain("sk-");
});

test("remote browser LaunchAgent keeps helper override optional and XML-safe", () => {
  process.env.CODEX_CHATGPT_WEB_HOME = "/Users/test/.codex-chatgpt-web";
  const definition = remoteBrowserServiceDefinition({
    target: "ubuntu@example.test",
    remoteDescriptorPath: "~/remote/browser.json",
    localDescriptorPath: "/Users/test/Library/Application Support/remote&browser.json",
    browserHelperScriptPath: "/Users/test/runtime/browser-helper.cjs",
    runtimeCommand: [process.execPath, "/opt/codex-chatgpt-web/app/cli.js"],
  });

  expect(definition).toContain("<string>--browser-helper-script</string>");
  expect(definition).toContain("<string>/Users/test/runtime/browser-helper.cjs</string>");
  expect(definition).toContain(
    "<string>/Users/test/Library/Application Support/remote&amp;browser.json</string>",
  );
  expect(definition).toContain("<string>~/remote/browser.json</string>");
});
