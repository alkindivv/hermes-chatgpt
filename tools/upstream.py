#!/usr/bin/env python3
"""Upstream drift review. Candidate checkout/fetch is confined to a separate clone.
No baseline acceptance, application update or service execution.

The initial lock is reviewed source data. check and update-inspect both fail on any
unreviewed drift; update-inspect emits the candidate inventory for manual review.
"""
import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
PIN = "8645a89e6a5fbe011e1bfbb0821a386ca7477ad5"


def git(source, *args):
    return subprocess.check_output(["git", "-C", str(source), *args], stderr=subprocess.PIPE).decode().strip()


def preload_inventory(text):
    start = re.search(r'contextBridge\.exposeInMainWorld\("([^"]+)",\s*\{', text)
    if not start:
        raise ValueError("Missing exposed preload object")
    body = text[start.end():]
    methods = list(re.finditer(r'^  (\w+):\s*', body, re.M))
    inventory = {}
    for i, method in enumerate(methods):
        segment = body[method.end():methods[i + 1].start() if i + 1 < len(methods) else len(body)]
        calls = re.findall(r'(ipcRenderer\.(?:invoke|send)|subscription)\(\s*"([^"]+)"', segment)
        if len(calls) != 1:
            raise ValueError("Unrecognized preload method: " + method[1])
        kind, channel = calls[0]
        inventory[method[1]] = {"kind": kind.removeprefix("ipcRenderer."), "channel": channel}
    if not inventory:
        raise ValueError("Empty preload API")
    return {"global": start[1], "methods": inventory}


def inventory(source):
    source = pathlib.Path(source).resolve()
    commit = git(source, "rev-parse", "HEAD")
    paths = subprocess.check_output(["git", "-C", str(source), "ls-files", "-z"]).decode().split("\0")
    # Include untracked additions: a new source/API file must not be silently missed.
    additions = subprocess.check_output(["git", "-C", str(source), "ls-files", "--others", "--exclude-standard", "-z"]).decode().split("\0")
    files = {}
    for name in sorted(set(paths + additions) - {""}):
        path = source / name
        if not path.is_file() or path.is_symlink():
            raise ValueError("Missing, nonregular or symlink source file: " + name)
        files[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"version": 1, "tag": "v6.1.10" if commit == PIN else None, "sourceSha": commit,
            "files": files, "preload": preload_inventory((source / "launcher/electron/preload.cjs").read_text())}


def differences(expected, actual):
    changes = []
    for field in ("version", "tag", "sourceSha", "preload"):
        if expected.get(field) != actual.get(field):
            changes.append(field)
    before, after = expected.get("files", {}), actual["files"]
    for path in sorted(before.keys() | after.keys()):
        if before.get(path) != after.get(path):
            changes.append("file:" + path)
    return changes


def digest(content):
    return hashlib.sha256(content).hexdigest()


def cef_guard(source, expected, patch):
    """Replay the locked patch only in a temporary directory, never in vendor."""
    source, patch = pathlib.Path(source), pathlib.Path(patch)
    changed = []
    if git(source, "rev-parse", "HEAD") != expected["sourceSha"]:
        changed.append("cef:sourceSha")
    if digest(patch.read_bytes()) != expected["patchSha256"]:
        changed.append("cef:patchSha256")
    original = subprocess.check_output(["git", "-C", str(source), "show", "HEAD:Cargo.toml"])
    if digest(original) != expected["upstreamManifestSha256"]:
        changed.append("cef:upstreamManifestSha256")
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        for name in expected["files"]:
            relative = pathlib.PurePosixPath(name)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("Invalid locked CEF path: " + name)
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(subprocess.check_output(["git", "-C", str(source), "show", "HEAD:" + name]))
        replay = subprocess.run(["git", "apply", "--check", str(patch.resolve())], cwd=directory, capture_output=True)
        if replay.returncode:
            changed.append("cef:patch-does-not-replay")
        else:
            subprocess.run(["git", "apply", str(patch.resolve())], cwd=directory, check=True, capture_output=True)
            replayed = {str(path.relative_to(root)): digest(path.read_bytes())
                        for path in root.rglob("*") if path.is_file()}
            for name in sorted(replayed.keys() | expected["files"].keys()):
                if replayed.get(name) != expected["files"].get(name):
                    changed.append("cef:replayed-file-hash:" + name)
    paths = subprocess.check_output(["git", "-C", str(source), "ls-files", "-z"]).decode().split("\0")
    paths += subprocess.check_output(["git", "-C", str(source), "ls-files", "--others", "--exclude-standard", "-z"]).decode().split("\0")
    actual = {}
    for name in set(paths) - {""}:
        path = source / name
        actual[name] = digest(path.read_bytes()) if path.is_file() and not path.is_symlink() else None
    for name in sorted(actual.keys() | expected["files"].keys()):
        if actual.get(name) != expected["files"].get(name):
            changed.append("cef:file:" + name)
    return changed


def parity_domain(path):
    name = path.lower()
    if "limits" in name:
        return "limits-estimates"
    if any(part in name for part in ("chatgpt-web-model", "model-catalog", "responses", "prompt-compiler", "broker", "mcp", "server", "sse")) and not name.startswith("launcher/"):
        return "responses-mcp-model-policy"
    if any(part in name for part in ("integration", "supervisor", "runtime", "tunnel", "service", "profile", "install", "update")):
        return "runtime-integration-updates"
    if any(part in name for part in ("zero-risk", "manual", "saved-chat", "compaction", "bigger-context")):
        return "interaction-context"
    if any(part in name for part in ("browser", "chatgpt-session", "chatgpt-web/", "passkey")):
        return "browser-account-turns"
    if name.startswith("launcher/"):
        return "launcher-state-ui"
    if name.startswith(("docs/", "scripts/", ".github/")) or name in ("license", "package.json", "bun.lock") or "readme" in name:
        return "upstream-maintenance-review"
    return "unmapped-review-required"


def touched_modules(expected, actual):
    before, after = expected["files"], actual["files"]
    return [{"path": path, "parityDomain": parity_domain(path),
             "beforeSha256": before.get(path), "afterSha256": after.get(path)}
            for path in sorted(before.keys() | after.keys()) if before.get(path) != after.get(path)]


def inspect_new_sha(source, sha, checkout, fetch_from=None):
    """Copy repository objects and inspect a detached candidate; no source ref changes."""
    if not re.fullmatch(r"[a-fA-F0-9]{40}", sha):
        raise ValueError("--new-sha must be a full 40-hex commit SHA")
    source, checkout = pathlib.Path(source).resolve(), pathlib.Path(checkout).resolve()
    if checkout.exists() or checkout == source or source in checkout.parents:
        raise ValueError("Candidate checkout must be a new path outside the source checkout")
    subprocess.run(["git", "clone", "--no-local", "--no-checkout", str(source), str(checkout)], check=True, capture_output=True)
    if fetch_from:
        subprocess.run(["git", "-C", str(checkout), "fetch", "--no-tags", "--", fetch_from, sha], check=True, capture_output=True)
    resolved = git(checkout, "rev-parse", "--verify", sha + "^{commit}")
    if resolved.lower() != sha.lower():
        raise ValueError("Candidate SHA did not resolve exactly")
    subprocess.run(["git", "-C", str(checkout), "-c", "core.hooksPath=/dev/null", "checkout", "--detach", resolved], check=True, capture_output=True)
    return checkout


class DriftTests(unittest.TestCase):
    def test_read_only_drift_in_temporary_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            p = pathlib.Path(directory)
            subprocess.run(["git", "init", "-q", str(p)], check=True)
            preload = p / "launcher/electron/preload.cjs"
            preload.parent.mkdir(parents=True)
            preload.write_text('contextBridge.exposeInMainWorld("api", {\n  snapshot: () => ipcRenderer.invoke("launcher:snapshot"),\n});\n')
            source = p / "state.cjs"
            source.write_text("original")
            subprocess.run(["git", "-C", str(p), "add", "."], check=True)
            subprocess.run(["git", "-C", str(p), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
            baseline = inventory(p)
            self.assertEqual(differences(baseline, inventory(p)), [])
            source.write_text("mutated")
            self.assertIn("file:state.cjs", differences(baseline, inventory(p)))
            source.write_text("original")
            preload.write_text(preload.read_text().replace('});', '  added: () => ipcRenderer.send("launcher:added"),\n});'))
            self.assertIn("preload", differences(baseline, inventory(p)))
            self.assertIn("added", inventory(p)["preload"]["methods"])
            lock = p / "lock.json"
            lock.write_text(json.dumps(baseline))
            before = lock.read_bytes()
            result = subprocess.run([sys.executable, __file__, "check", "--source", str(p), "--lock", str(lock)], capture_output=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn(b"preload", result.stderr)
            self.assertIn(b"file:launcher/electron/preload.cjs", result.stderr)
            self.assertEqual(before, lock.read_bytes())
            result = subprocess.run([sys.executable, __file__, "update-inspect", "--source", str(p), "--lock", str(lock)], capture_output=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn("preload", json.loads(result.stdout)["unreviewedChanges"])
            self.assertEqual(before, lock.read_bytes())

    def test_candidate_sha_in_separate_clone_and_domain_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            preload = source / "launcher/electron/preload.cjs"
            preload.parent.mkdir(parents=True)
            preload.write_text('contextBridge.exposeInMainWorld("api", {\n  snapshot: () => ipcRenderer.invoke("launcher:snapshot"),\n});\n')
            state = source / "launcher/electron/state.cjs"
            state.write_text("baseline")
            def commit():
                subprocess.run(["git", "-C", str(source), "add", "."], check=True)
                subprocess.run(["git", "-C", str(source), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
                return git(source, "rev-parse", "HEAD")
            commit()
            baseline = inventory(source)
            state.write_text("changed policy")
            sha = commit()
            source_status = git(source, "status", "--porcelain")
            candidate = inspect_new_sha(source, sha, root / "candidate")
            actual = inventory(candidate)
            report = touched_modules(baseline, actual)
            self.assertEqual(actual["sourceSha"], sha)
            self.assertEqual(report[0]["path"], "launcher/electron/state.cjs")
            self.assertEqual(report[0]["parityDomain"], "launcher-state-ui")
            self.assertNotEqual(report[0]["beforeSha256"], report[0]["afterSha256"])
            self.assertIn("sourceSha", differences(baseline, actual))
            self.assertEqual(git(source, "rev-parse", "HEAD"), sha)
            self.assertEqual(git(source, "status", "--porcelain"), source_status)
            with self.assertRaises(ValueError):
                inspect_new_sha(source, sha, candidate)
            self.assertEqual(parity_domain("src/model-catalog.ts"), "responses-mcp-model-policy")
            self.assertEqual(parity_domain("launcher/electron/limits-store.cjs"), "limits-estimates")
            self.assertEqual(parity_domain("new-unknown-source.ts"), "unmapped-review-required")

    def test_cef_guard_replays_patch_and_rejects_unreviewed_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "cef"
            subprocess.run(["git", "init", "-q", str(source)], check=True)
            fixture_paths = ["Cargo.toml", "crates/gpui-cef/src/platform/cef/mod.rs"]
            originals = {name: subprocess.check_output(["git", "-C", str(ROOT / "vendor/gpui-cef"), "show", "HEAD:" + name])
                         for name in fixture_paths}
            for name, content in originals.items():
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            original = originals["Cargo.toml"]
            manifest = source / "Cargo.toml"
            subprocess.run(["git", "-C", str(source), "add", "."], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
            patch = root / "pin.patch"
            patch.write_bytes((ROOT / "tools/patches/gpui-cef-pin.patch").read_bytes())
            subprocess.run(["git", "-C", str(source), "apply", str(patch)], check=True)
            expected = {"sourceSha": git(source, "rev-parse", "HEAD"), "patchSha256": digest(patch.read_bytes()),
                        "upstreamManifestSha256": digest(original), "files": {name: digest((source / name).read_bytes()) for name in fixture_paths}}
            before = manifest.read_bytes()
            zoom = source / "crates/gpui-cef/src/platform/cef/mod.rs"
            zoom_before = zoom.read_bytes()
            self.assertIn(b"pub fn set_zoom_factor", zoom_before)
            self.assertEqual(cef_guard(source, expected, patch), [])
            zoom.write_bytes(zoom_before.replace(b"factor.ln()", b"factor.log10()"))
            self.assertIn("cef:file:crates/gpui-cef/src/platform/cef/mod.rs", cef_guard(source, expected, patch))
            zoom.write_bytes(zoom_before)
            wrong = json.loads(json.dumps(expected))
            wrong["files"]["crates/gpui-cef/src/platform/cef/mod.rs"] = "0" * 64
            self.assertIn("cef:replayed-file-hash:crates/gpui-cef/src/platform/cef/mod.rs", cef_guard(source, wrong, patch))
            self.assertEqual(manifest.read_bytes(), before)
            manifest.write_bytes(before + b"# unreviewed\n")
            self.assertIn("cef:file:Cargo.toml", cef_guard(source, expected, patch))
            manifest.write_bytes(before)
            patch.write_bytes(patch.read_bytes() + b"\n")
            self.assertIn("cef:patchSha256", cef_guard(source, expected, patch))
            self.assertEqual(manifest.read_bytes(), before)
            patch.write_text("not a valid patch\n")
            self.assertIn("cef:patch-does-not-replay", cef_guard(source, expected, patch))
            self.assertEqual(zoom.read_bytes(), zoom_before)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["check", "update-inspect", "cef-guard", "test"])
    parser.add_argument("--source", type=pathlib.Path, default=ROOT / "vendor/upstream")
    parser.add_argument("--lock", type=pathlib.Path, default=ROOT / "upstream.lock.json")
    parser.add_argument("--cef-source", type=pathlib.Path, default=ROOT / "vendor/gpui-cef")
    parser.add_argument("--cef-patch", type=pathlib.Path, default=ROOT / "tools/patches/gpui-cef-pin.patch")
    parser.add_argument("--new-sha", help="Full commit SHA to inspect in a separate detached clone")
    parser.add_argument("--checkout", type=pathlib.Path, help="New, nonexistent candidate directory; otherwise temporary")
    parser.add_argument("--fetch-from", help="Explicit repository to fetch the SHA into the candidate clone only")
    args = parser.parse_args()
    if args.command == "test":
        return 0 if unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(DriftTests)).wasSuccessful() else 1
    if (args.new_sha or args.checkout or args.fetch_from) and args.command != "update-inspect":
        parser.error("Candidate options are only valid with update-inspect")
    if (args.checkout or args.fetch_from) and not args.new_sha:
        parser.error("--checkout/--fetch-from require --new-sha")
    temporary = None
    try:
        expected = json.loads(args.lock.read_text())
        cef_changes = cef_guard(args.cef_source, expected["cef"], args.cef_patch) if "cef" in expected else ["cef:missing-lock-guard"]
        if args.command == "cef-guard":
            print(json.dumps({"unreviewedChanges": cef_changes}, indent=2))
            return 1 if cef_changes else 0
        source = args.source
        if args.new_sha:
            if args.checkout:
                candidate_path = args.checkout
            else:
                temporary = tempfile.TemporaryDirectory(prefix="codex-upstream-inspect-")
                candidate_path = pathlib.Path(temporary.name) / "candidate"
            source = inspect_new_sha(source, args.new_sha, candidate_path, args.fetch_from)
        actual = inventory(source)
        changed = differences(expected, actual) + cef_changes
        if actual["sourceSha"] != PIN:
            changed.append("reference pin differs from v6.1.10")
        if args.command == "update-inspect":
            print(json.dumps({"unreviewedChanges": changed, "candidate": actual,
                              "candidateCheckout": str(pathlib.Path(source).resolve()),
                              "candidateRetained": bool(args.new_sha and args.checkout),
                              "touchedModules": touched_modules(expected, actual),
                              "cefPatchGuard": {"passed": not cef_changes, "changes": cef_changes}}, indent=2))
        elif changed:
            print("Unreviewed upstream drift:\n" + "\n".join(changed), file=sys.stderr)
        else:
            print(f"Upstream verified: {actual['sourceSha']}; {len(actual['files'])} files; {len(actual['preload']['methods'])} preload methods; CEF patch guard passed")
        return 1 if changed else 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
        print("Upstream inspection failed: " + str(exc), file=sys.stderr)
        return 1
    finally:
        if temporary:
            temporary.cleanup()


if __name__ == "__main__":
    sys.exit(main())
