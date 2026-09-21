"""Contract tests against the real, separately installed official Hermes checkout.

Run with the official checkout on PYTHONPATH and Python >= 3.11. No user profile,
remote model, account token or live Hermes service is used by these tests.
"""
from __future__ import annotations

import copy
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent


class ProviderContractTest(unittest.TestCase):
    def test_00_distribution_exists(self):
        self.assertTrue((ROOT / "model-provider" / "__init__.py").is_file())
        self.assertTrue((ROOT / "install.py").is_file())

    def test_10_real_discovery_profile_isolation_and_transport(self):
        with tempfile.TemporaryDirectory(prefix="hermes-provider-test-") as temp:
            home_a = Path(temp) / "A"
            home_b = Path(temp) / "B"
            home_a.mkdir()
            home_b.mkdir()
            with patch.dict(os.environ, {"HERMES_HOME": str(home_a), "HOME": temp, "HERMES_CHATGPT_API_KEY": "test-only-key"}):
                result = subprocess.run([sys.executable, str(ROOT / "install.py"), "--hermes-home", str(home_a)], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                from providers import get_provider_profile
                from agent.transports.chat_completions import ChatCompletionsTransport
                profile = get_provider_profile("hermes-chatgpt")
                self.assertIsNotNone(profile)
                self.assertEqual(profile.api_mode, "chat_completions")
                self.assertEqual(profile.get_hostname(), "")
                self.assertEqual(
                    profile.fallback_models,
                    (
                        "chatgpt-web/light",
                        "chatgpt-web/medium",
                        "chatgpt-web/high",
                        "chatgpt-web/extra-high",
                        "chatgpt-web/pro",
                    ),
                )
                self.assertEqual(profile.model_capabilities["chatgpt-web/high"].get("context_window"), 90000)
                self.assertTrue(profile.model_capabilities["chatgpt-web/high"].get("supports_tools"))
                with patch(
                    "agent.relay_runtime.current_turn",
                    return_value=SimpleNamespace(
                        turn_id="relay-turn-native-1",
                        lease=SimpleNamespace(session_id="same-session"),
                    ),
                ):
                    one = profile.build_extra_body(session_id="same-session")
                    self.assertEqual(one["hermes"]["turn_id"], "relay-turn-native-1")
                    self.assertNotIn("turn_id", profile.build_extra_body())
                    self.assertNotIn(
                        "turn_id",
                        profile.build_extra_body(session_id="different-session")["hermes"],
                    )
                self.assertEqual(one["hermes"]["session_id"], "same-session")
                with patch.dict(os.environ, {"HERMES_HOME": str(home_b)}):
                    two = profile.build_extra_body(session_id="same-session")
                three = profile.build_extra_body(session_id="same-session")
                self.assertNotEqual(one["hermes"]["profile_id"], two["hermes"]["profile_id"])
                self.assertEqual(
                    {k: v for k, v in one["hermes"].items() if k != "turn_id"},
                    three["hermes"],
                )
                from hermes_constants import set_hermes_home_override, reset_hermes_home_override
                scope = set_hermes_home_override(home_b)
                try:
                    self.assertEqual(profile.build_extra_body(session_id="same-session"), two)
                finally:
                    reset_hermes_home_override(scope)
                self.assertEqual(profile.build_extra_body(session_id="same-session"), three)
                from agent.models_dev import get_model_capabilities
                capabilities = get_model_capabilities("hermes-chatgpt", "chatgpt-web/high", allow_network=False)
                self.assertEqual(capabilities.context_window, 90000)
                self.assertTrue(capabilities.supports_tools)
                self.assertNotIn("session_id", profile.build_extra_body()["hermes"])
                messages = [{"role": "system", "content": "Unchanged prompt"}, {"role": "user", "content": "Use memory"}]
                original = copy.deepcopy(messages)
                # This is the official transport, not a fake provider-specific serializer.
                transport = ChatCompletionsTransport()
                kwargs = transport.build_kwargs(messages=messages, model="chatgpt-web/high", tools=[{"type": "function", "function": {"name": "memory", "parameters": {"type": "object"}}}],
                    provider_profile=profile, provider="hermes-chatgpt", session_id="main-session", base_url=profile.base_url)
                self.assertEqual(kwargs["extra_body"]["hermes"]["session_id"], "main-session")
                self.assertEqual(kwargs["model"], "chatgpt-web/high")
                self.assertEqual(kwargs["tools"][0]["function"]["name"], "memory")
                self.assertEqual(messages, original)
                self.assertEqual(profile.default_aux_model, "chatgpt-web/medium")
                self.assertEqual(
                    profile.classify_api_error(
                        Exception("stream error"),
                        status_code=None,
                        error_code="rate_limit_exceeded",
                        message="ChatGPT rate limit",
                        body={"code": "rate_limit_exceeded"},
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "upstream_rate_limit",
                        "retryable": True,
                        "should_fallback": True,
                        "should_rotate_credential": False,
                    },
                )
                from agent.error_classifier import classify_api_error
                stream_error = Exception("stream error")
                stream_error.body = {
                    "message": "ChatGPT rate limit",
                    "type": "rate_limit_error",
                    "code": "rate_limit_exceeded",
                    "retryable": True,
                }
                classified = classify_api_error(
                    stream_error,
                    provider="hermes-chatgpt",
                    model="chatgpt-web/high",
                )
                self.assertEqual(classified.reason.value, "upstream_rate_limit")
                self.assertTrue(classified.retryable)
                self.assertTrue(classified.should_fallback)
                self.assertFalse(classified.should_rotate_credential)

                self.assertEqual(
                    profile.classify_api_error(
                        Exception("ambiguous"),
                        status_code=None,
                        error_code="chatgpt_submission_ambiguous",
                        message="ChatGPT did not confirm that the prompt was sent",
                        body=None,
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "format_error",
                        "retryable": False,
                        "should_fallback": False,
                        "should_rotate_credential": False,
                    },
                )
                self.assertEqual(
                    profile.classify_api_error(
                        Exception("model control"),
                        status_code=None,
                        error_code="upstream_server_error",
                        message="ChatGPT model controls are unavailable",
                        body={"code": "upstream_server_error", "retryable": False},
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "server_error",
                        "retryable": False,
                        "should_fallback": True,
                        "should_rotate_credential": False,
                    },
                )
                self.assertEqual(
                    profile.classify_api_error(
                        Exception("connector"),
                        status_code=None,
                        error_code="connector_not_found",
                        message="Codex Native connector missing",
                        body={"code": "connector_not_found", "retryable": False},
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "server_error",
                        "retryable": False,
                        "should_fallback": True,
                        "should_rotate_credential": False,
                    },
                )
                self.assertEqual(
                    profile.classify_api_error(
                        Exception("capacity"),
                        status_code=503,
                        error_code="chatgpt_browser_capacity",
                        message="all browser tabs are busy",
                        body={"code": "chatgpt_browser_capacity", "retryable": True},
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "overloaded",
                        "retryable": True,
                        "should_fallback": True,
                        "should_rotate_credential": False,
                    },
                )
                self.assertEqual(
                    profile.classify_api_error(
                        Exception("future structured failure"),
                        status_code=502,
                        error_code="future_bridge_code",
                        message="future bridge failure",
                        body={"code": "future_bridge_code", "retryable": False},
                        model="chatgpt-web/high",
                    ),
                    {
                        "reason": "server_error",
                        "retryable": False,
                        "should_fallback": True,
                        "should_rotate_credential": False,
                    },
                )
                refused = subprocess.run(
                    [sys.executable, str(ROOT / "install.py"), "--hermes-home", str(home_a)],
                    capture_output=True,
                    text=True,
                )
                self.assertNotEqual(refused.returncode, 0)
                upgraded = subprocess.run(
                    [
                        sys.executable,
                        str(ROOT / "install.py"),
                        "--hermes-home",
                        str(home_a),
                        "--upgrade",
                    ],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(upgraded.returncode, 0, upgraded.stderr)
                self.assertIn("upgraded", upgraded.stdout)
                self.assertFalse((home_a / "config.yaml").exists())

    def test_20_streaming_bridge_errors_reach_native_hermes_classifier(self):
        bun = os.environ.get("HERMES_TEST_BUN")
        self.assertTrue(bun, "Set HERMES_TEST_BUN to the pinned Bun runtime")
        token = "error-classifier-test-" + "x" * 48
        env = {**os.environ, "HERMES_CHATGPT_TEST_KEY": token}
        process = subprocess.Popen(
            [bun, str(ROOT / "error-classifier-fixture.ts")],
            cwd=ROOT.parent.parent,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            assert process.stdout is not None
            port = None
            lines = []
            for line in process.stdout:
                lines.append(line)
                if line.startswith("HERMES_TEST_PORT="):
                    port = int(line.split("=", 1)[1])
                    break
            self.assertIsNotNone(port, "fixture did not start:\n" + "".join(lines))

            from openai import OpenAI
            from agent.error_classifier import classify_api_error
            from providers import get_provider_profile

            profile = get_provider_profile("hermes-chatgpt")
            live_models = profile.fetch_models(
                api_key=token,
                base_url=f"http://127.0.0.1:{port}/v1",
            )
            self.assertEqual(
                live_models,
                [
                    "chatgpt-web/light",
                    "chatgpt-web/medium",
                    "chatgpt-web/high",
                    "chatgpt-web/extra-high",
                    "chatgpt-web/pro",
                ],
            )

            client = OpenAI(
                api_key=token,
                base_url=f"http://127.0.0.1:{port}/v1",
                max_retries=0,
            )
            caught = None
            try:
                stream = client.chat.completions.create(
                    model="chatgpt-web/high",
                    messages=[{"role": "user", "content": "trigger a rate limit"}],
                    stream=True,
                )
                for _ in stream:
                    pass
            except Exception as exc:
                caught = exc
            finally:
                client.close()

            self.assertIsNotNone(caught, "OpenAI SDK unexpectedly treated the error frame as success")
            self.assertEqual(getattr(caught, "body", {}).get("code"), "rate_limit_exceeded")
            self.assertIs(getattr(caught, "body", {}).get("retryable"), True)
            classified = classify_api_error(
                caught,
                provider="hermes-chatgpt",
                model="chatgpt-web/high",
                base_url=f"http://127.0.0.1:{port}/v1",
            )
            self.assertEqual(classified.reason.value, "upstream_rate_limit")
            self.assertTrue(classified.retryable)
            self.assertTrue(classified.should_fallback)
            self.assertFalse(classified.should_rotate_credential)

            def stream_failure(prompt: str):
                probe = OpenAI(
                    api_key=token,
                    base_url=f"http://127.0.0.1:{port}/v1",
                    max_retries=0,
                )
                failure = None
                try:
                    stream = probe.chat.completions.create(
                        model="chatgpt-web/high",
                        messages=[{"role": "user", "content": prompt}],
                        stream=True,
                    )
                    for _ in stream:
                        pass
                except Exception as exc:
                    failure = exc
                finally:
                    probe.close()
                self.assertIsNotNone(failure)
                return failure

            overflow = stream_failure("HERMES_CONTEXT_OVERFLOW_CONTRACT")
            self.assertEqual(getattr(overflow, "body", {}).get("code"), "context_length_exceeded")
            overflow_class = classify_api_error(
                overflow,
                provider="hermes-chatgpt",
                model="chatgpt-web/high",
                base_url=f"http://127.0.0.1:{port}/v1",
            )
            self.assertEqual(overflow_class.reason.value, "context_overflow")
            self.assertTrue(overflow_class.retryable)
            self.assertTrue(overflow_class.should_compress)
            self.assertFalse(overflow_class.should_fallback)

            submitted = stream_failure("HERMES_SUBMITTED_FAILURE_CONTRACT")
            self.assertEqual(
                getattr(submitted, "body", {}).get("code"),
                "chatgpt_submitted_turn_failed",
            )
            submitted_class = classify_api_error(
                submitted,
                provider="hermes-chatgpt",
                model="chatgpt-web/high",
                base_url=f"http://127.0.0.1:{port}/v1",
            )
            self.assertEqual(submitted_class.reason.value, "format_error")
            self.assertFalse(submitted_class.retryable)
            self.assertFalse(submitted_class.should_fallback)

            from agent.auxiliary_client import call_llm
            route_info = {}
            auxiliary = call_llm(
                task="contract_probe",
                provider="hermes-chatgpt",
                model=None,
                base_url=f"http://127.0.0.1:{port}/v1",
                api_key=token,
                messages=[
                    {
                        "role": "user",
                        "content": "HERMES_AUXILIARY_CONTRACT reply exactly once",
                    }
                ],
                timeout=15,
                route_info=route_info,
            )
            self.assertEqual(auxiliary.choices[0].message.content, "HERMES_AUXILIARY_OK")
            self.assertEqual(route_info.get("model"), "chatgpt-web/medium")
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            if process.stdout:
                process.stdout.close()

    def test_30_native_profile_sync_removes_only_legacy_context_pins(self):
        bun = os.environ.get("HERMES_TEST_BUN")
        self.assertTrue(bun, "Set HERMES_TEST_BUN to the pinned Bun runtime")
        token = "profile-sync-test-" + "x" * 48
        process = subprocess.Popen(
            [bun, str(ROOT / "error-classifier-fixture.ts")],
            cwd=ROOT.parent.parent,
            env={**os.environ, "HERMES_CHATGPT_TEST_KEY": token},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            assert process.stdout is not None
            port = None
            lines = []
            for line in process.stdout:
                lines.append(line)
                if line.startswith("HERMES_TEST_PORT="):
                    port = int(line.split("=", 1)[1])
                    break
            self.assertIsNotNone(port, "fixture did not start:\n" + "".join(lines))

            with tempfile.TemporaryDirectory(prefix="hermes-native-sync-") as temp:
                home = Path(temp)
                plugin = home / "plugins" / "model-providers" / "hermes-chatgpt"
                plugin.parent.mkdir(parents=True)
                shutil.copytree(ROOT / "model-provider", plugin)
                config = home / "config.yaml"
                config.write_text(
                    "model:\n"
                    "  provider: hermes-chatgpt\n"
                    "  default: chatgpt-web/high\n"
                    "  context_length: 90000\n"
                    "model_overrides:\n"
                    "  hermes-chatgpt:\n"
                    "    chatgpt-web/light: {context_window: 41000, supports_tools: true, supports_vision: true, supports_reasoning: false}\n"
                    "    chatgpt-web/medium: {context_window: 90000, supports_tools: true, supports_vision: true, supports_reasoning: false}\n"
                    "    chatgpt-web/high: {context_window: 90000, supports_tools: true, supports_vision: true, supports_reasoning: false}\n"
                    "    chatgpt-web/extra-high: {context_window: 90000, supports_tools: true, supports_vision: true, supports_reasoning: false}\n"
                    "    chatgpt-web/pro: {context_window: 104000, supports_tools: true, supports_vision: true, supports_reasoning: false}\n"
                    "auxiliary:\n"
                    "  compression:\n"
                    "    provider: auto\n"
                    "    model: ''\n"
                    "    base_url: ''\n"
                    "    api_key: '[REDACTED]'\n"
                    "    timeout: 120\n"
                    "unrelated:\n"
                    "  keep: true\n"
                )
                key = home / "bridge.key"
                key.write_text(token)
                key.chmod(0o600)
                result = subprocess.run(
                    [
                        sys.executable,
                        str(ROOT / "sync_profile.py"),
                        "--hermes-home",
                        str(home),
                        "--api-key-file",
                        str(key),
                        "--base-url",
                        f"http://127.0.0.1:{port}/v1",
                    ],
                    env={**os.environ, "PYTHONPATH": "/usr/local/lib/hermes-agent"},
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

                import yaml
                migrated = yaml.safe_load(config.read_text())
                self.assertNotIn("context_length", migrated["model"])
                self.assertEqual(migrated["model"]["default"], "chatgpt-web/high")
                self.assertNotIn("hermes-chatgpt", migrated.get("model_overrides", {}))
                self.assertEqual(migrated["auxiliary"]["compression"]["provider"], "auto")
                self.assertEqual(migrated["auxiliary"]["compression"]["model"], "")
                self.assertEqual(migrated["auxiliary"]["compression"]["api_key"], "[REDACTED]")
                self.assertEqual(migrated["auxiliary"]["compression"]["timeout"], 120)
                self.assertTrue(migrated["unrelated"]["keep"])
                backups = list(home.glob("config.yaml.before-hermes-chatgpt-native-*"))
                self.assertEqual(len(backups), 1)

                cache = yaml.safe_load((home / "context_length_cache.yaml").read_text())
                cached = cache["context_lengths"]
                suffix = f"@http://127.0.0.1:{port}/v1"
                self.assertEqual(cached["chatgpt-web/high" + suffix], 111193)
                self.assertEqual(cached["chatgpt-web/pro" + suffix], 112193)

                # Idempotent re-runs refresh live metadata but do not rewrite config or create
                # another migration backup once legacy pins are gone.
                second = subprocess.run(
                    [
                        sys.executable,
                        str(ROOT / "sync_profile.py"),
                        "--hermes-home",
                        str(home),
                        "--api-key-file",
                        str(key),
                        "--base-url",
                        f"http://127.0.0.1:{port}/v1",
                    ],
                    env={**os.environ, "PYTHONPATH": "/usr/local/lib/hermes-agent"},
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(second.returncode, 0, second.stderr)
                self.assertEqual(
                    len(list(home.glob("config.yaml.before-hermes-chatgpt-native-*"))),
                    1,
                )
                migrated_again = yaml.safe_load(config.read_text())
                self.assertEqual(migrated_again, migrated)
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            if process.stdout:
                process.stdout.close()

    def test_40_legacy_provider_profile_without_capability_field_still_loads(self):
        with tempfile.TemporaryDirectory(prefix="hermes-provider-legacy-") as temp:
            root = Path(temp)
            providers = root / "providers"
            providers.mkdir()
            (providers / "__init__.py").write_text(
                "REGISTRY = []\n"
                "def register_provider(profile): REGISTRY.append(profile)\n"
            )
            (providers / "base.py").write_text(
                "from dataclasses import dataclass, field\n"
                "class _Omit: pass\n"
                "OMIT_TEMPERATURE = _Omit()\n"
                "@dataclass\n"
                "class ProviderProfile:\n"
                "    name: str\n"
                "    api_mode: str = 'chat_completions'\n"
                "    display_name: str = ''\n"
                "    description: str = ''\n"
                "    env_vars: tuple = ()\n"
                "    base_url: str = ''\n"
                "    supports_vision: bool = False\n"
                "    supports_vision_tool_messages: bool = True\n"
                "    fixed_temperature: object = None\n"
                "    default_aux_model: str = ''\n"
                "    fallback_models: tuple = ()\n"
            )
            script = (
                "import importlib.util, sys; "
                f"sys.path.insert(0, {str(root)!r}); "
                f"spec=importlib.util.spec_from_file_location('legacy_plugin', {str(ROOT / 'model-provider' / '__init__.py')!r}); "
                "mod=importlib.util.module_from_spec(spec); spec.loader.exec_module(mod); "
                "from providers import REGISTRY; "
                "p=REGISTRY[-1]; "
                "assert p.name == 'hermes-chatgpt'; "
                "assert p.base_url.endswith('/v1'); "
                "assert not hasattr(p, 'model_capabilities')"
            )
            result = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
