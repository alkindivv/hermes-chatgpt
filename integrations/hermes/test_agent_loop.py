"""Offline end-to-end test with real Hermes AIAgent and production bridge/broker.

Only ChatGPT's browser is simulated. Native Hermes writes/reads a temporary file,
runs printf, updates its own todo store and writes temporary profile memory.
Run from an environment containing the official Hermes checkout and its deps.
"""
from __future__ import annotations

from collections import deque
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent


class NativeAgentLoopTest(unittest.TestCase):
    def test_native_tools_round_trip_through_production_bridge(self):
        bun = os.environ.get("HERMES_TEST_BUN") or shutil.which("bun")
        if not bun:
            self.fail("Set HERMES_TEST_BUN to the pinned Bun 1.4.0 executable")
        with tempfile.TemporaryDirectory(prefix="hermes-e2e-") as temp:
            root = Path(temp)
            home = root / "profile"
            home.mkdir()
            plugin = home / "plugins" / "model-providers" / "hermes-chatgpt"
            shutil.copytree(ROOT / "model-provider", plugin)
            token = "offline-test-key-" + "x" * 48
            env = {**os.environ, "HERMES_CHATGPT_TEST_KEY": token}
            logs: deque[str] = deque(maxlen=300)
            ready: queue.Queue[int] = queue.Queue()
            process = subprocess.Popen(
                [bun, str(ROOT / "agent-loop-fixture.ts"), temp],
                cwd=REPO, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                text=True,
            )
            def read_output():
                assert process.stdout is not None
                for line in process.stdout:
                    logs.append(line)
                    if line.startswith("HERMES_TEST_PORT="):
                        ready.put(int(line.split("=", 1)[1]))
            thread = threading.Thread(target=read_output, daemon=True)
            thread.start()
            try:
                port = ready.get(timeout=20)
                (home / "config.yaml").write_text(
                    "terminal:\n  backend: local\n  cwd: " + temp + "\n"
                    "agent:\n  max_turns: 12\n"
                    "approvals:\n  mode: manual\n  deny:\n    - 'touch *'\n", encoding="utf-8")
                with patch.dict(os.environ, {
                    "HOME": temp, "HERMES_HOME": str(home),
                    "TERMINAL_ENV": "local", "TERMINAL_CWD": temp,
                    "HERMES_CHATGPT_API_KEY": token,
                }):
                    from run_agent import AIAgent
                    # No subclass, patched dispatcher, fake OpenAI client, or custom
                    # tool runner: every tool passes through Hermes's actual loop.
                    agent = AIAgent(
                        provider="hermes-chatgpt", model="chatgpt-web/high",
                        base_url=f"http://127.0.0.1:{port}/v1", api_key=token,
                        api_mode="chat_completions", session_id="native-bridge-contract",
                        enabled_toolsets=["file", "terminal", "todo", "memory"],
                        max_iterations=12, run_budget_seconds=45, quiet_mode=True,
                        skip_context_files=True, skip_background_review=True,
                        cwd=temp,
                    )
                    try:
                        result = agent.run_conversation(
                            "Exercise the temporary native tool loop.",
                            system_message="Hermes transport contract test. Operate only inside the temporary test directory.",
                        )
                        self.assertIn("HERMES_NATIVE_LOOP_OK browser_starts=1", result.get("final_response", ""), str(result))
                        self.assertEqual((root / "proof.txt").read_text(), "HERMES_NATIVE_FILE")
                        messages = result["messages"]
                        names = []
                        count = 0
                        for m in messages:
                            for call in m.get("tool_calls", []):
                                count += 1
                                function = call["function"]
                                if function["name"] == "tool_describe":
                                    continue
                                if function["name"] == "tool_call":
                                    names.extend(c["name"] for c in json.loads(function["arguments"])["calls"])
                                else:
                                    names.append(function["name"])
                        self.assertEqual(names, ["write_file", "read_file", "terminal", "todo_list", "memory", "terminal"])
                        self.assertFalse((root / "denied.txt").exists(), "Denied command unexpectedly executed")
                        self.assertEqual(sum(m["role"] == "tool" for m in messages), count)
                        memory = list(home.rglob("MEMORY.md"))
                        self.assertTrue(any("HERMES_BACKEND_TEST_ONLY" in p.read_text() for p in memory))
                    finally:
                        agent.close()
            except BaseException:
                print("\n--- offline bridge fixture ---\n" + "".join(logs))
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                thread.join(timeout=2)
                if process.stdout:
                    process.stdout.close()


if __name__ == "__main__":
    unittest.main()
