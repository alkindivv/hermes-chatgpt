import { defaultBrokerEndpoint, resolveBrokerEndpoint } from "../../config";
import { runChatGptMcpServer, type ChatGptMcpContract } from "./mcp-server";
import { runHermesMcpServer } from "../../hermes/mcp";

function option(args: string[], name: string, fallback: string): string {
  const index = args.indexOf(name);
  if (index < 0) return fallback;
  const value = args[index + 1]?.trim();
  if (!value) throw new Error(`${name} requires a value`);
  args.splice(index, 2);
  return value;
}

export async function runChatGptMcpMain(args: string[]): Promise<void> {
  const remaining = [...args];
  const brokerSocketPath = resolveBrokerEndpoint(option(remaining, "--broker-socket", defaultBrokerEndpoint()));
  const backend = option(remaining, "--backend", "codex");
  if (backend !== "codex" && backend !== "hermes") throw new Error("--backend must be codex or hermes");
  const requestedContract = option(remaining, "--contract", "native");
  if (requestedContract !== "native" && requestedContract !== "safe") {
    throw new Error(`--contract must be native or safe, received ${requestedContract}`);
  }
  if (remaining.length > 0) throw new Error(`Unknown MCP arguments: ${remaining.join(" ")}`);
  if (backend === "hermes") {
    if (requestedContract !== "native") throw new Error("Hermes supports automatic interaction only");
    await runHermesMcpServer(brokerSocketPath);
    return;
  }
  await runChatGptMcpServer({
    brokerSocketPath,
    contract: requestedContract as ChatGptMcpContract,
  });
}
