#!/usr/bin/env python3
"""Install the local Hermes ChatGPT bridge token into a Hermes credential pool.

The token is read from a private file so it never needs to appear in argv,
stdout, shell history, or a profile .env file.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import stat
import uuid


def _read_private_token(path: Path) -> str:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_ISLNK(info.st_mode):
        raise SystemExit(f"Credential source must be a regular non-symlink file: {path}")
    if info.st_mode & 0o077:
        raise SystemExit(f"Credential source must not be group/world accessible: {path}")
    token = path.read_text(encoding="utf-8").strip()
    if len(token) < 40 or len(token) > 512:
        raise SystemExit("Credential source is empty or has an unexpected length")
    return token


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", type=Path, required=True)
    parser.add_argument("--api-key-file", type=Path, required=True)
    parser.add_argument("--provider", default="hermes-chatgpt")
    parser.add_argument("--label", default="local-chatgpt-bridge")
    args = parser.parse_args()

    home = args.hermes_home.expanduser().resolve()
    key_file = args.api_key_file.expanduser().resolve()
    if not home.is_dir():
        raise SystemExit(f"Hermes profile home does not exist: {home}")

    # Hermes 0.21.2 scopes credential-pool persistence from HERMES_HOME.
    os.environ["HERMES_HOME"] = str(home)

    from agent.credential_pool import (
        AUTH_TYPE_API_KEY,
        PooledCredential,
        load_pool,
    )
    from providers import get_provider_profile

    token = _read_private_token(key_file)
    profile = get_provider_profile(args.provider)
    if profile is None or not str(getattr(profile, "base_url", "") or "").strip():
        raise SystemExit(f"Provider {args.provider!r} has no configured base URL")
    base_url = str(profile.base_url).strip().rstrip("/")
    pool = load_pool(args.provider)

    # This provider is dedicated to one local bridge instance. Remove any
    # env-seeded or stale rows first so rotation cannot leave an old token ahead
    # of the current owner-only file in fill_first selection.
    while pool.entries():
        pool.remove_index(1)

    pool.add_entry(PooledCredential(
        provider=args.provider,
        id=uuid.uuid4().hex[:8],
        label=args.label,
        auth_type=AUTH_TYPE_API_KEY,
        priority=0,
        source="manual:hermes-chatgpt-file",
        access_token=token,
        base_url=base_url,
    ))
    print(f"Credential installed for {args.provider} in {home} (token not printed).")


if __name__ == "__main__":
    main()
