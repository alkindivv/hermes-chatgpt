#!/usr/bin/env python3
"""Migrate one Hermes profile from legacy ChatGPT-Web pins to native provider discovery.

This intentionally touches only configuration written by older hermes-chatgpt installers:
- model.context_length when it equals the old hardcoded window for the selected chatgpt-web model
- model_overrides.hermes-chatgpt when the whole block equals the old generated capability table

Everything else is preserved. A timestamped backup is written before the first config mutation.
The live bridge is then queried through Hermes' own model-metadata resolver so its endpoint-scoped
context cache is populated exactly as it would be for any other local/OpenAI-compatible provider.
"""
from __future__ import annotations

import argparse
from copy import deepcopy
from datetime import datetime, timezone
import os
from pathlib import Path
import shutil
import stat
import sys


LEGACY_CONTEXTS = {
    "chatgpt-web/light": 41_000,
    "chatgpt-web/medium": 90_000,
    "chatgpt-web/high": 90_000,
    "chatgpt-web/extra-high": 90_000,
    "chatgpt-web/pro": 104_000,
}
LEGACY_OVERRIDES = {
    model: {
        "context_window": context,
        "supports_tools": True,
        "supports_vision": True,
        "supports_reasoning": False,
    }
    for model, context in LEGACY_CONTEXTS.items()
}


def _private_token(path: Path) -> str:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise SystemExit(f"Bridge key must be a regular non-symlink file: {path}")
    if info.st_mode & 0o077:
        raise SystemExit(f"Bridge key must not be group/world accessible: {path}")
    token = path.read_text(encoding="utf-8").strip()
    if not 40 <= len(token) <= 512:
        raise SystemExit("Bridge key has an unexpected length")
    return token


def _plain(value):
    if isinstance(value, dict):
        return {str(k): _plain(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_plain(v) for v in value]
    return value


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", type=Path, required=True)
    parser.add_argument("--api-key-file", type=Path, required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:17842/v1")
    args = parser.parse_args()

    home = args.hermes_home.expanduser().resolve()
    config_path = home / "config.yaml"
    if not home.is_dir() or not config_path.is_file():
        raise SystemExit(f"Hermes profile/config does not exist: {home}")
    if config_path.is_symlink():
        raise SystemExit(f"Refusing symlinked config: {config_path}")
    token = _private_token(args.api_key_file.expanduser().resolve())
    base_url = args.base_url.strip().rstrip("/")
    if not base_url.startswith("http://127.0.0.1:") and not base_url.startswith("http://localhost:"):
        raise SystemExit("Native profile sync is restricted to a loopback bridge URL")

    from ruamel.yaml import YAML

    yaml = YAML()
    yaml.preserve_quotes = True
    with config_path.open("r", encoding="utf-8") as handle:
        config = yaml.load(handle) or {}
    if not isinstance(config, dict):
        raise SystemExit(f"Invalid Hermes config root: {config_path}")

    before = deepcopy(config)
    changes: list[str] = []

    model_cfg = config.get("model")
    if isinstance(model_cfg, dict) and str(model_cfg.get("provider") or "") == "hermes-chatgpt":
        model_key = "default" if model_cfg.get("default") is not None else "model"
        model_id = str(model_cfg.get(model_key) or "")
        legacy = LEGACY_CONTEXTS.get(model_id)
        pinned = model_cfg.get("context_length")
        if legacy is not None and pinned == legacy:
            del model_cfg["context_length"]
            changes.append(f"removed legacy model.context_length={legacy}")
        elif pinned is not None:
            changes.append(f"preserved user model.context_length={pinned}")
    # Preserve auxiliary routing exactly. Hermes owns `auto` semantics and may intentionally
    # resolve compression/background tasks to the current main runtime or a user-declared route.
    overrides = config.get("model_overrides")
    if isinstance(overrides, dict) and "hermes-chatgpt" in overrides:
        existing = _plain(overrides["hermes-chatgpt"])
        if existing == LEGACY_OVERRIDES:
            del overrides["hermes-chatgpt"]
            changes.append("removed legacy model_overrides.hermes-chatgpt")
            if not overrides:
                del config["model_overrides"]
        else:
            changes.append("preserved customized model_overrides.hermes-chatgpt")

    mutated = _plain(config) != _plain(before)
    backup = None
    if mutated:
        stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
        backup = config_path.with_name(f"config.yaml.before-hermes-chatgpt-native-{stamp}")
        shutil.copy2(config_path, backup)
        temp = config_path.with_name(f".{config_path.name}.hermes-chatgpt-native-{os.getpid()}")
        try:
            with temp.open("w", encoding="utf-8") as handle:
                yaml.dump(config, handle)
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(temp, stat.S_IMODE(config_path.stat().st_mode))
            os.replace(temp, config_path)
        finally:
            temp.unlink(missing_ok=True)

    # Seed/verify Hermes' own endpoint-scoped context cache using the same resolver the
    # runtime uses at agent initialization. This remains live-reconcilable for loopback endpoints.
    os.environ["HERMES_HOME"] = str(home)
    from hermes_constants import set_hermes_home_override, reset_hermes_home_override
    from agent.model_metadata import get_model_context_length, save_context_length

    scope = set_hermes_home_override(home)
    try:
        resolved = {
            model: get_model_context_length(
                model,
                base_url=base_url,
                api_key=token,
                provider="hermes-chatgpt",
            )
            for model in LEGACY_CONTEXTS
        }
        if any(not isinstance(value, int) or value <= 0 for value in resolved.values()):
            raise SystemExit(f"Bridge context discovery returned invalid values: {resolved}")
        for model, value in resolved.items():
            save_context_length(model, base_url, value, source="hermes-chatgpt-live")
    finally:
        reset_hermes_home_override(scope)

    print(f"Profile synchronized: {home}")
    for change in changes or ["no legacy config pins found"]:
        print(f"- {change}")
    if backup is not None:
        print(f"- backup: {backup}")
    print("- live contexts: " + ", ".join(f"{model}={value}" for model, value in resolved.items()))


if __name__ == "__main__":
    main()
