#!/usr/bin/env python3
"""Check that a desktop release tag matches the packaged application version."""

import argparse
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path


def package_json_version(root):
    data = json.loads((root / "apps/desktop/package.json").read_text(encoding="utf-8"))
    version = data.get("version")
    if not isinstance(version, str) or not version:
        raise ValueError("apps/desktop/package.json has no version")
    return version


def tauri_version(root):
    data = json.loads(
        (root / "apps/desktop/src-tauri/tauri.conf.json").read_text(encoding="utf-8")
    )
    version = data.get("version")
    if not isinstance(version, str) or not version:
        raise ValueError("apps/desktop/src-tauri/tauri.conf.json has no version")
    return version


def cargo_package_version(text):
    match = re.search(
        r'(?ms)^\[package\]\n.*?\nversion = "([^"]+)"',
        text,
    )
    if not match:
        raise ValueError("apps/desktop/src-tauri/Cargo.toml has no package version")
    return match.group(1)


def cargo_lock_version(text):
    for block in text.split("[[package]]"):
        if re.search(r'(?m)^name = "astrlink-desktop"$', block):
            match = re.search(r'(?m)^version = "([^"]+)"', block)
            if match:
                return match.group(1)
            break
    raise ValueError("Cargo.lock has no astrlink-desktop package version")


def desktop_versions(root):
    return {
        "apps/desktop/package.json": package_json_version(root),
        "apps/desktop/src-tauri/tauri.conf.json": tauri_version(root),
        "apps/desktop/src-tauri/Cargo.toml": cargo_package_version(
            (root / "apps/desktop/src-tauri/Cargo.toml").read_text(encoding="utf-8")
        ),
        "apps/desktop/src-tauri/Cargo.lock": cargo_lock_version(
            (root / "apps/desktop/src-tauri/Cargo.lock").read_text(encoding="utf-8")
        ),
    }


def check_tag(root, tag):
    versions = desktop_versions(root)
    unique = set(versions.values())
    if len(unique) != 1:
        details = ", ".join(f"{path}={version}" for path, version in versions.items())
        raise ValueError(f"desktop version fields disagree: {details}")
    version = unique.pop()
    expected = f"v{version}"
    if tag != expected:
        raise ValueError(
            f"release tag {tag} does not match desktop version {version}; expected {expected}"
        )
    return version


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--tag")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ReleaseTagTest)
        result = unittest.TextTestRunner(verbosity=1).run(suite)
        return 0 if result.wasSuccessful() else 1
    if not args.tag:
        parser.error("--tag is required")
    try:
        version = check_tag(args.root, args.tag)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print(f"{args.tag} matches desktop version {version}")
    return 0


class ReleaseTagTest(unittest.TestCase):
    def write_tree(self, root, version, lock_version=None):
        package = root / "apps/desktop"
        tauri = package / "src-tauri"
        tauri.mkdir(parents=True)
        (package / "package.json").write_text(
            json.dumps({"name": "@astrlink/desktop", "version": version}),
            encoding="utf-8",
        )
        (tauri / "tauri.conf.json").write_text(
            json.dumps({"productName": "AstrLink", "version": version}),
            encoding="utf-8",
        )
        (tauri / "Cargo.toml").write_text(
            f'[package]\nname = "astrlink-desktop"\nversion = "{version}"\n\n'
            '[dependencies]\nserde = { version = "=1.0.0" }\n',
            encoding="utf-8",
        )
        locked = lock_version or version
        (tauri / "Cargo.lock").write_text(
            "[[package]]\n"
            'name = "serde"\n'
            'version = "1.0.0"\n\n'
            "[[package]]\n"
            'name = "astrlink-desktop"\n'
            f'version = "{locked}"\n',
            encoding="utf-8",
        )

    def test_matching_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_tree(root, "0.2.0")
            self.assertEqual(check_tag(root, "v0.2.0"), "0.2.0")

    def test_rejects_different_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_tree(root, "0.2.0")
            with self.assertRaises(ValueError):
                check_tag(root, "v0.1.0")

    def test_rejects_disagreement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_tree(root, "0.2.0", lock_version="0.1.0")
            with self.assertRaises(ValueError):
                check_tag(root, "v0.2.0")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
