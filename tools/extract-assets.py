#!/usr/bin/env python3
"""Regenerate pinned UI data; explicit Node22+ strips pure TypeScript at build time.

No launcher or application runtime is executed. Existing unrelated assets are
never deleted. --check compares exact bytes and SHA256 without writing.
"""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parents[1]
LANGUAGES = ("en", "zh-CN", "zh-TW", "ja", "ko")


def load_upstream():
    spec = importlib.util.spec_from_file_location("upstream", ROOT / "tools/upstream.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def generate(source, node):
    source = source.resolve()
    upstream = load_upstream()
    expected = json.loads((ROOT / "upstream.lock.json").read_text())
    actual = upstream.inventory(source)
    changes = upstream.differences(expected, actual)
    if actual["sourceSha"] != upstream.PIN or changes:
        raise ValueError("Refusing to execute unreviewed i18n source: " + ", ".join(changes))
    executable = pathlib.Path(node).expanduser().resolve(strict=True)
    version = subprocess.check_output([str(executable), "--version"], text=True).strip()
    if not re.fullmatch(r"v\d+\.\d+\.\d+", version) or int(version[1:].split(".")[0]) < 22:
        raise ValueError("Explicit --node must point to Node22+ with --experimental-strip-types support")
    program = '''
const { copyFor } = await import(process.argv[1]);
const { limitsCopyFor } = await import(process.argv[2]);
const languages = ["en", "zh-CN", "zh-TW", "ja", "ko"];
console.log(JSON.stringify({copy: Object.fromEntries(languages.map(l => [l, copyFor(l)])),
 limits: Object.fromEntries(languages.map(l => [l, limitsCopyFor(l)]))}));
'''
    result = subprocess.run([str(executable), "--experimental-strip-types", "--input-type=module", "-e", program,
                             (source / "launcher/src/i18n.ts").as_uri(),
                             (source / "launcher/src/limits-copy.ts").as_uri()],
                            check=True, capture_output=True, text=True)
    data = json.loads(result.stdout)
    encode = lambda v: (json.dumps(v, ensure_ascii=False, indent=2) + "\n").encode()
    languages = json.loads((source / "launcher/electron/languages.json").read_text())
    if tuple(languages) != LANGUAGES or tuple(data["copy"]) != LANGUAGES or tuple(data["limits"]) != LANGUAGES:
        raise ValueError("Language inventory changed; review generator")
    files = {"copy.json": encode(data["copy"]), "limits-copy.json": encode(data["limits"]),
             "languages.json": encode(languages), "UPSTREAM-LICENSE": (source / "LICENSE").read_bytes()}
    icons = (source / "launcher/src/icons.tsx").read_text()
    names = re.findall(r'\| "([a-z-]+)"', icons.split("export function Icon", 1)[0])
    common = 'fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.7"'
    matches = re.findall(r'\{name === "([a-z-]+)" \? (.*?) : null\}', icons)
    if set(names) != {name for name, _ in matches} or len(matches) != len(names):
        raise ValueError("Icon JSX changed; review extraction")
    for name, jsx in matches:
        body = jsx.replace("<>", "").replace("</>", "").replace("{...common}", common)
        svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">' + body + '</svg>\n'
        if "{" in svg or "}" in svg:
            raise ValueError("Unrecognized dynamic icon JSX: " + name)
        ET.fromstring(svg)
        files[f"icons/{name}.svg"] = svg.encode()
    app = (source / "launcher/src/App.tsx").read_text()
    brand = re.search(r'function BrandMark\(.*?(<svg.*?</svg>)', app, re.S)
    if not brand:
        raise ValueError("Missing BrandMark SVG")
    svg = brand[1].replace('aria-hidden="true"', 'xmlns="http://www.w3.org/2000/svg"') + "\n"
    ET.fromstring(svg)
    files["icons/brand.svg"] = svg.encode()
    return files


def hashes(files):
    return {name: hashlib.sha256(content).hexdigest() for name, content in sorted(files.items())}


def compare(files, output):
    return [name for name, content in files.items() if not (output / name).is_file() or (output / name).read_bytes() != content]


def write(files, output):
    for name, content in files.items():
        path = output / name
        if path.is_file() and path.read_bytes() == content:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)


class AssetTests(unittest.TestCase):
    node = None

    def test_pinned_regeneration_hashes_roundtrip_and_drift(self):
        source = ROOT / "vendor/upstream"
        generated = generate(source, self.node)
        # Byte equality, not semantic JSON equality: preserve all existing copy/SVG bytes.
        self.assertEqual(compare(generated, ROOT / "assets"), [])
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory)
            write(generated, output)
            first = hashes({name: (output / name).read_bytes() for name in generated})
            write(generate(source, self.node), output)
            self.assertEqual(first, hashes({name: (output / name).read_bytes() for name in generated}))
            (output / "copy.json").write_text("tampered")
            self.assertEqual(compare(generated, output), ["copy.json"])
            write(generated, output)
            self.assertEqual(compare(generated, output), [])
        self.assertEqual(tuple(json.loads(generated["limits-copy.json"])), LANGUAGES)
        self.assertEqual(len([p for p in generated if p.endswith(".svg")]), 25)
        self.assertEqual(generated["UPSTREAM-LICENSE"], (source / "LICENSE").read_bytes())

    def test_unreviewed_source_rejected_before_node_execution(self):
        upstream = load_upstream()
        with tempfile.TemporaryDirectory() as directory:
            source = upstream.inspect_new_sha(ROOT / "vendor/upstream", upstream.PIN, pathlib.Path(directory) / "source")
            path = source / "launcher/src/i18n.ts"
            path.write_text(path.read_text() + "\nthrow new Error('must never execute');\n")
            with self.assertRaisesRegex(ValueError, "Refusing to execute unreviewed"):
                generate(source, "/nonexistent-node-must-not-run")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", required=True, help="Explicit path to Node22+ (not used by the Rust app)")
    parser.add_argument("--source", type=pathlib.Path, default=ROOT / "vendor/upstream")
    parser.add_argument("--output", type=pathlib.Path, default=ROOT / "assets")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--test", action="store_true")
    args = parser.parse_args()
    try:
        if args.test:
            AssetTests.node = args.node
            return 0 if unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(AssetTests)).wasSuccessful() else 1
        files = generate(args.source, args.node)
        changed = compare(files, args.output)
        if not args.check:
            write(files, args.output)
        print(json.dumps({"mode": "check" if args.check else "generate", "changed": changed,
                          "sha256": hashes(files)}, indent=2))
        return 1 if args.check and changed else 0
    except (OSError, ValueError, subprocess.SubprocessError, ET.ParseError) as exc:
        print("Asset extraction failed: " + str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
