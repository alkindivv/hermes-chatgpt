#!/usr/bin/env python3
"""Install only the provider plugin; never edit Hermes configuration or credentials."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import sys


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", type=Path, help="Target Hermes profile home")
    args = parser.parse_args()
    home = (args.hermes_home or Path(os.environ.get("HERMES_HOME", "~/.hermes"))).expanduser().resolve()
    destination = home / "plugins" / "model-providers" / "hermes-chatgpt"
    source = Path(__file__).resolve().parent / "model-provider"
    if destination.exists() or destination.is_symlink():
        raise SystemExit(f"Refusing to overwrite existing plugin: {destination}")
    for parent in (home / "plugins", home / "plugins" / "model-providers"):
        if parent.is_symlink():
            raise SystemExit(f"Refusing a symlinked plugin directory: {parent}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    print(f"Provider plugin installed: {destination}")
    print("Existing config.yaml, .env, sessions and running services were not modified.")
    print("Restart a new Hermes process after configuring the bridge URL and local API token.")


if __name__ == "__main__":
    try:
        main()
    except OSError as exc:
        print(f"Plugin installation failed: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
