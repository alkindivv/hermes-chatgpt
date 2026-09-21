"""ChatGPT browser transport for Hermes via the official provider-plugin API.

Install outside the Hermes source tree. The bridge only supplies model responses;
Hermes keeps its normal tool dispatcher, approvals, memory and conversation loop.
"""
from __future__ import annotations

import hashlib
from typing import Any

from providers import register_provider
from providers.base import OMIT_TEMPERATURE, ProviderProfile


def _classify_api_error(
    error: Exception,
    *,
    status_code: int | None = None,
    error_code: str | None = None,
    message: str = "",
    body: Any = None,
    model: str | None = None,
) -> dict[str, Any] | None:
    # Streaming ChatGPT failures happen after HTTP 200 headers are committed, so the
    # OpenAI SDK can surface them as a generic APIError with no useful status. Preserve
    # the bridge's structured code as the provider-native recovery signal.
    code = str(error_code or "").strip().lower()
    if not code and isinstance(body, dict):
        candidate = body.get("code")
        if candidate is None and isinstance(body.get("error"), dict):
            candidate = body["error"].get("code")
        if isinstance(candidate, str):
            code = candidate.strip().lower()
    text = str(message or error or "").lower()
    structured_retryable = (
        body.get("retryable")
        if isinstance(body, dict) and isinstance(body.get("retryable"), bool)
        else None
    )

    if code == "rate_limit_exceeded" or "chatgpt rate limit" in text:
        return {
            "reason": "rate_limit",
            "retryable": True,
            "should_fallback": True,
            "should_rotate_credential": False,
        }
    if code == "chatgpt_session_expired" or "session has expired" in text:
        return {
            "reason": "auth_permanent",
            "retryable": False,
            "should_fallback": True,
            "should_rotate_credential": False,
        }
    if code == "context_length_exceeded":
        return {
            "reason": "context_overflow",
            "retryable": True,
            "should_compress": True,
            "should_fallback": False,
        }
    if code in {
        "chatgpt_subscription_unavailable",
        "chatgpt_browser_capacity",
        "chatgpt_browser_unavailable",
        "chatgpt_session_registry_full",
    }:
        return {
            "reason": "overloaded",
            "retryable": True,
            "should_fallback": True,
            "should_rotate_credential": False,
        }
    if code in {"upstream_server_error", "hermes_backend_error"}:
        retryable = True if structured_retryable is None else structured_retryable
        return {
            "reason": "server_error",
            "retryable": retryable,
            "should_fallback": True,
            "should_rotate_credential": False,
        }
    if code == "chatgpt_stopped_thinking":
        # The UI does not expose a stable machine-readable reason. Do not replay the
        # same web turn; allow an explicitly configured fallback route instead.
        return {
            "reason": "upstream_rate_limit",
            "retryable": False,
            "should_fallback": True,
            "should_rotate_credential": False,
        }
    if code in {
        "chatgpt_submission_ambiguous",
        "chatgpt_submitted_turn_failed",
        "client_cancelled",
        "compaction_control_unavailable",
        "compaction_handoff_failed",
        "compaction_handoff_timeout",
        "compaction_source_unavailable",
        "invalid_output_schema",
        "manual_handoff_timeout",
        "manual_multipart_unsupported",
        "manual_turn_cancelled",
        "multipart_protocol_violation",
        "prompt_attachment_integrity",
        "structured_output_validation_failed",
        "too_many_attachments",
    }:
        # These are deterministic or side-effect-ambiguous. Retrying/falling back could
        # duplicate work after Send was already activated.
        return {
            "reason": "format_error",
            "retryable": False,
            "should_fallback": False,
            "should_rotate_credential": False,
        }

    if code in {
        "browser_interaction_mode_mismatch",
        "browser_stream_inconsistent",
        "connector_not_found",
        "manual_launcher_failed",
    }:
        # Provider/runtime misconfiguration cannot heal by replaying the same browser route,
        # but a user-declared fallback provider may still complete the task.
        return {
            "reason": "server_error",
            "retryable": False,
            "should_fallback": True,
            "should_rotate_credential": False,
        }

    # Last-resort preservation of a structured bridge verdict. Any future bridge code that
    # carries an explicit retryable flag should not silently degrade to Hermes' generic
    # status/message classifier while the plugin is one release behind.
    if code and structured_retryable is not None:
        if status_code == 401:
            reason = "auth_permanent"
        elif status_code == 429:
            reason = "rate_limit"
        elif status_code in {503, 529}:
            reason = "overloaded"
        elif status_code is not None and 400 <= status_code < 500:
            reason = "format_error"
        else:
            reason = "server_error"
        return {
            "reason": reason,
            "retryable": structured_retryable,
            "should_fallback": structured_retryable or reason in {
                "auth_permanent",
                "overloaded",
                "server_error",
            },
            "should_rotate_credential": False,
        }
    return None


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
            # Hermes 0.21.3 exposes one stable Relay turn id across every API/tool
            # round of the same human turn, including in-place context compression.
            # Carry it only on session-bound main requests. Auxiliary calls invoke
            # build_extra_body without session_id and remain intentionally stateless.
            try:
                from agent.relay_runtime import current_turn
                turn = current_turn()
                lease = getattr(turn, "lease", None)
                turn_id = str(getattr(turn, "turn_id", "") or "").strip()
                if (
                    turn_id
                    and str(getattr(lease, "session_id", "") or "") == session_id
                ):
                    metadata["turn_id"] = turn_id
            except Exception:
                # Older Hermes releases have no Relay turn context; the bridge keeps
                # its content-derived fallback identity for those installations.
                pass
        # Auxiliary requests without a session are explicitly stateless. The
        # bridge refuses tool-enabled requests that lack session provenance.
        return {"hermes": metadata}

    def supported_reasoning_efforts(self, model: str) -> tuple[str, ...]:
        # The public model slug, not a second effort control, selects browser mode.
        return ()

    def fetch_models(
        self,
        *,
        api_key: str | None = None,
        base_url: str | None = None,
        timeout: float = 8.0,
    ) -> list[str] | None:
        # Model discovery doubles as provider health. The bridge's verified catalog proves the
        # authenticated ChatGPT browser surface while preserving the standard ProviderProfile API.
        # Never forward the local bridge credential to a non-loopback endpoint.
        import json
        from urllib.parse import urlparse
        import urllib.request

        effective = str(base_url or self.base_url or "").strip().rstrip("/")
        if not effective:
            return None
        try:
            parsed = urlparse(effective)
        except ValueError:
            return None
        if parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}:
            return None
        from hermes_cli.urllib_security import open_credentialed_url

        request = urllib.request.Request(effective + "/models/verified")
        if api_key:
            request.add_header("Authorization", f"Bearer {api_key}")
        request.add_header("Accept", "application/json")
        try:
            with open_credentialed_url(request, timeout=timeout) as response:
                payload = json.loads(response.read().decode("utf-8"))
            items = payload.get("data", []) if isinstance(payload, dict) else []
            return [
                item["id"]
                for item in items
                if isinstance(item, dict) and isinstance(item.get("id"), str)
            ]
        except Exception:
            return None

    def build_client_kwargs_extras(self, **context: Any) -> dict[str, Any]:
        # Hermes owns retries/failover. Avoid an extra SDK retry loop on side effects.
        return {"max_retries": 0}

_profile_kwargs = dict(
    name="hermes-chatgpt",
    api_mode="chat_completions",
    display_name="ChatGPT Web (Hermes bridge)",
    description="Local authenticated browser bridge; Hermes retains native tools and approvals",
    env_vars=("HERMES_CHATGPT_API_KEY",),
    base_url="http://127.0.0.1:17842/v1",
    supports_vision=True,
    supports_vision_tool_messages=True,
    fixed_temperature=OMIT_TEMPERATURE,
    # Hermes reserves the provider aux model for compression, review, vision helpers,
    # memory/session summarization and similar side work. Medium keeps a large context window
    # without burning the heavier high/extra-high browser effort tier on every side task.
    default_aux_model="chatgpt-web/medium",
    fallback_models=(
        "chatgpt-web/light",
        "chatgpt-web/medium",
        "chatgpt-web/high",
        "chatgpt-web/extra-high",
        "chatgpt-web/pro",
    ),
)

# Hermes >= 0.21.3 lets provider plugins declare per-model capabilities directly.
# Older supported installs (including 0.21.2) use config.yaml model_overrides.
_profile_fields = getattr(ProviderProfile, "__dataclass_fields__", {})

if "classify_api_error" in _profile_fields:
    _profile_kwargs["classify_api_error"] = _classify_api_error

if "model_capabilities" in _profile_fields:
    # Conservative offline fallbacks for inventory/capability consumers. Runtime context sizing
    # is discovered from the bridge's live loopback /models metadata after legacy config pins are
    # removed; the live endpoint can therefore raise/lower the actual account-specific window.
    _profile_kwargs["model_capabilities"] = {
        model: {"context_window": window, "supports_vision": True,
                "supports_tools": True, "supports_reasoning": False}
        for model, window in (
            ("chatgpt-web/light", 41000),
            ("chatgpt-web/medium", 90000),
            ("chatgpt-web/high", 90000),
            ("chatgpt-web/extra-high", 90000),
            ("chatgpt-web/pro", 104000),
        )
    }

register_provider(HermesChatGPTProfile(**_profile_kwargs))
