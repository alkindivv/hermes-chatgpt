"""Contract tests against the real, separately installed official Hermes checkout.

Run with the official checkout on PYTHONPATH and Python >= 3.11. No user profile,
remote model, account token or live Hermes service is used by these tests.
"""
from __future__ import annotations

import copy
import os
from pathlib import Path
import subprocess
import sys
import tempfile
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
                self.assertEqual(profile.model_capabilities["chatgpt-web/high"].get("context_window"), 90000)
                self.assertTrue(profile.model_capabilities["chatgpt-web/high"].get("supports_tools"))
                one = profile.build_extra_body(session_id="same-session")
                self.assertEqual(one["hermes"]["session_id"], "same-session")
                with patch.dict(os.environ, {"HERMES_HOME": str(home_b)}):
                    two = profile.build_extra_body(session_id="same-session")
                three = profile.build_extra_body(session_id="same-session")
                self.assertNotEqual(one["hermes"]["profile_id"], two["hermes"]["profile_id"])
                self.assertEqual(one, three)
                from hermes_constants import set_hermes_home_override, reset_hermes_home_override
                scope = set_hermes_home_override(home_b)
                try:
                    self.assertEqual(profile.build_extra_body(session_id="same-session"), two)
                finally:
                    reset_hermes_home_override(scope)
                self.assertEqual(profile.build_extra_body(session_id="same-session"), one)
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
                refused = subprocess.run([sys.executable, str(ROOT / "install.py"), "--hermes-home", str(home_a)], capture_output=True, text=True)
                self.assertNotEqual(refused.returncode, 0)
                self.assertFalse((home_a / "config.yaml").exists())

    def test_20_legacy_provider_profile_without_capability_field_still_loads(self):
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
