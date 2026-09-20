#!/usr/bin/env python3
"""Configure Hermes title generation to use 9Router's 9R-Light model.

The 9Router API key is read from an existing trusted Hermes profile and stored
in the target profile's credential pool. The key is never printed or copied
into target config.yaml.
"""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import uuid

import yaml

PROVIDER_NAME = "9Router-Light"
MODEL_NAME = "9R-Light"
DEFAULT_BASE_URL = "https://api.alkindi.id/v1"
POOL_LABEL = "9router-light"


def _read_yaml(path: Path) -> dict:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise SystemExit(f"Config source must be a regular non-symlink file: {path}")
    data = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(data, dict):
        raise SystemExit(f"Config is not a mapping: {path}")
    return data


def _source_credential(source_config: Path, base_url: str) -> str:
    cfg = _read_yaml(source_config)
    keys = {
        str(entry.get("api_key") or "").strip()
        for entry in (cfg.get("custom_providers") or [])
        if isinstance(entry, dict)
        and str(entry.get("base_url") or "").strip().rstrip("/") == base_url.rstrip("/")
        and str(entry.get("api_key") or "").strip()
    }
    if not keys:
        raise SystemExit(f"No 9Router API key found for {base_url} in {source_config}")
    if len(keys) != 1:
        raise SystemExit(f"Multiple different 9Router API keys found in {source_config}; refusing ambiguity")
    token = next(iter(keys))
    if len(token) < 20 or len(token) > 4096:
        raise SystemExit("9Router API key has an unexpected length")
    return token


def _upsert_provider(existing: object, base_url: str) -> list[dict]:
    providers = [dict(x) for x in existing] if isinstance(existing, list) else []
    providers = [
        item for item in providers
        if str(item.get("name") or "").strip().lower() != PROVIDER_NAME.lower()
    ]
    providers.append({
        "name": PROVIDER_NAME,
        "api_mode": "chat_completions",
        "base_url": base_url.rstrip("/"),
        "model": MODEL_NAME,
        "models": {MODEL_NAME: {"context_length": 1_000_000}},
    })
    return providers


def _quiet_config_set(key: str, value: str) -> None:
    from hermes_cli.config import set_config_value
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        set_config_value(key, value)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", type=Path, required=True)
    parser.add_argument("--source-config", type=Path, required=True)
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    args = parser.parse_args()

    home = args.hermes_home.expanduser().resolve()
    source_config = args.source_config.expanduser().resolve()
    if not home.is_dir() or not (home / "config.yaml").is_file():
        raise SystemExit(f"Hermes profile home/config.yaml does not exist: {home}")

    base_url = str(args.base_url).strip().rstrip("/")
    token = _source_credential(source_config, base_url)

    # Scope all Hermes config + credential-pool operations to the target profile.
    os.environ["HERMES_HOME"] = str(home)

    target_cfg = _read_yaml(home / "config.yaml")
    providers = _upsert_provider(target_cfg.get("custom_providers"), base_url)
    _quiet_config_set("custom_providers", json.dumps(providers, separators=(",", ":")))
    _quiet_config_set("auxiliary.title_generation.provider", f"custom:{PROVIDER_NAME}")
    _quiet_config_set("auxiliary.title_generation.model", MODEL_NAME)
    _quiet_config_set("auxiliary.title_generation.timeout", "60")
    _quiet_config_set("auxiliary.title_generation.base_url", "")
    _quiet_config_set("auxiliary.title_generation.api_key", "")

    from agent.credential_pool import (
        AUTH_TYPE_API_KEY,
        PooledCredential,
        get_custom_provider_pool_key,
        load_pool,
    )

    pool_key = get_custom_provider_pool_key(base_url, PROVIDER_NAME)
    if not pool_key:
        raise SystemExit("Could not resolve the named custom provider credential-pool key")
    pool = load_pool(pool_key)
    while pool.entries():
        pool.remove_index(1)
    pool.add_entry(PooledCredential(
        provider=pool_key,
        id=uuid.uuid4().hex[:8],
        label=POOL_LABEL,
        auth_type=AUTH_TYPE_API_KEY,
        priority=0,
        source="manual:9router-title",
        access_token=token,
        base_url=base_url,
    ))

    print(
        f"9Router title generation configured in {home}: "
        f"provider=custom:{PROVIDER_NAME}, model={MODEL_NAME}, pool={pool_key} "
        "(API key not printed)."
    )


if __name__ == "__main__":
    main()
