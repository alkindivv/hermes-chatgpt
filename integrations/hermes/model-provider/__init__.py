"""ChatGPT browser transport for Hermes via the official provider-plugin API.

Install outside the Hermes source tree. The bridge only supplies model responses;
Hermes keeps its normal tool dispatcher, approvals, memory and conversation loop.
"""
from __future__ import annotations

import hashlib
from typing import Any

from providers import register_provider
from providers.base import OMIT_TEMPERATURE, ProviderProfile


class HermesChatGPTProfile(ProviderProfile):
    def get_hostname(self) -> str:
        # Do not claim every localhost provider through global URL reverse lookup.
        return ""

    def build_extra_body(
        self, *, session_id: str | None = None, **context: Any
    ) -> dict[str, Any]:
        # Resolve at request time: the same process may serve several profiles.
        # Do not cache HERMES_HOME or the active session on the provider singleton.
        from hermes_constants import get_hermes_home

        profile_id = hashlib.sha256(
            str(get_hermes_home().resolve()).encode("utf-8")
        ).hexdigest()
        metadata = {"profile_id": profile_id}
        if session_id:
            metadata["session_id"] = session_id
        # Auxiliary requests without a session are explicitly stateless. The
        # bridge refuses tool-enabled requests that lack session provenance.
        return {"hermes": metadata}

    def supported_reasoning_efforts(self, model: str) -> tuple[str, ...]:
        # The public model slug, not a second effort control, selects browser mode.
        return ()

    def build_client_kwargs_extras(self, **context: Any) -> dict[str, Any]:
        # Hermes owns retries/failover. Avoid an extra SDK retry loop on side effects.
        return {"max_retries": 0}


register_provider(HermesChatGPTProfile(
    name="hermes-chatgpt",
    api_mode="chat_completions",
    display_name="ChatGPT Web (Hermes bridge)",
    description="Local authenticated browser bridge; Hermes retains native tools and approvals",
    env_vars=("HERMES_CHATGPT_API_KEY",),
    base_url="http://127.0.0.1:17842/v1",
    supports_vision=True,
    supports_vision_tool_messages=True,
    fixed_temperature=OMIT_TEMPERATURE,
    default_aux_model="chatgpt-web/high",
    fallback_models=("chatgpt-web/light", "chatgpt-web/medium", "chatgpt-web/high"),
    # Conservative defaults, not guesses at the underlying model's advertised
    # maximum. Operator overrides may use the verified local /v1/models catalog.
    model_capabilities={
        model: {"context_window": window, "supports_vision": True,
                "supports_tools": True, "supports_reasoning": False}
        for model, window in (
            ("chatgpt-web/light", 41000),
            ("chatgpt-web/medium", 90000),
            ("chatgpt-web/high", 90000),
            ("chatgpt-web/extra-high", 90000),
            ("chatgpt-web/pro", 104000),
        )
    },
))
