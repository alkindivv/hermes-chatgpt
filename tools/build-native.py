#!/usr/bin/env python3
"""Build the isolated native target with its own CEF cache and Ninja executable."""
import argparse
import os
from pathlib import Path
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["check", "test", "build", "bundle"])
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    ninja = root / ".build-tools/bin/ninja"
    if not ninja.is_file():
        parser.error("Run python3 -m venv .build-tools, then .build-tools/bin/pip install ninja==1.13.2")
    sdk = subprocess.check_output(["xcrun", "--show-sdk-path"], text=True).strip()
    env = os.environ.copy()
    env["CEF_PATH"] = str(root / ".cef")
    env["CXXFLAGS"] = f"-nostdinc++ -isystem {sdk}/usr/include/c++/v1"
    env["PATH"] = str(ninja.parent) + os.pathsep + env.get("PATH", "")
    env["CARGO_BUILD_JOBS"] = "1"
    env["CARGO_NET_OFFLINE"] = "true"
    commands = {
        "check": ["cargo", "check", "-p", "codex-web-gpui"],
        "test": ["cargo", "test", "--workspace"],
        "build": ["cargo", "build", "-p", "codex-web-gpui", "--bins"],
        "bundle": ["cargo", "run", "-p", "codex-web-gpui", "--bin", "bundle-codex-web-gpui"],
    }
    return subprocess.run(commands[args.action] + ["--locked"], cwd=root, env=env).returncode


if __name__ == "__main__":
    sys.exit(main())
