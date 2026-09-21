#!/usr/bin/env python3
"""Install only the provider plugin; never edit Hermes configuration or credentials."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import sys
import uuid


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", type=Path, help="Target Hermes profile home")
    parser.add_argument(
        "--upgrade",
        action="store_true",
        help="Atomically replace an existing hermes-chatgpt provider plugin",
    )
    args = parser.parse_args()
    home = (args.hermes_home or Path(os.environ.get("HERMES_HOME", "~/.hermes"))).expanduser().resolve()
    destination = home / "plugins" / "model-providers" / "hermes-chatgpt"
    source = Path(__file__).resolve().parent / "model-provider"
    for parent in (home / "plugins", home / "plugins" / "model-providers"):
        if parent.is_symlink():
            raise SystemExit(f"Refusing a symlinked plugin directory: {parent}")
    destination.parent.mkdir(parents=True, exist_ok=True)

    exists = destination.exists() or destination.is_symlink()
    if exists and not args.upgrade:
        raise SystemExit(f"Refusing to overwrite existing plugin without --upgrade: {destination}")
    if destination.is_symlink() or (exists and not destination.is_dir()):
        raise SystemExit(f"Refusing to replace non-directory plugin path: {destination}")

    ignore = shutil.ignore_patterns("__pycache__", "*.pyc")
    if not exists:
        shutil.copytree(source, destination, ignore=ignore)
        action = "installed"
    else:
        nonce = uuid.uuid4().hex
        staged = destination.parent / f".hermes-chatgpt.stage-{nonce}"
        backup = destination.parent / f".hermes-chatgpt.backup-{nonce}"
        swapped = False
        try:
            shutil.copytree(source, staged, ignore=ignore)
            destination.rename(backup)
            try:
                staged.rename(destination)
                swapped = True
            except BaseException:
                # If rollback itself fails, leave the backup in place for manual recovery;
                # never delete the last known-good plugin in a cleanup clause.
                if not destination.exists() and backup.exists():
                    backup.rename(destination)
                raise
            if backup.exists():
                shutil.rmtree(backup)
        finally:
            shutil.rmtree(staged, ignore_errors=True)
            if swapped and backup.exists():
                shutil.rmtree(backup, ignore_errors=True)
        action = "upgraded"

    print(f"Provider plugin {action}: {destination}")
    print("Existing config.yaml, .env, sessions and running services were not modified.")
    print("Restart a new Hermes process after configuring the bridge URL and local API token.")


if __name__ == "__main__":
    try:
        main()
    except OSError as exc:
        print(f"Plugin installation failed: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
