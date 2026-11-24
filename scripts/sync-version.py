#!/usr/bin/env python3
"""Synchronize or verify AetherShare's version across all manifests from root VERSION.

Usage:
  python3 scripts/sync-version.py              # sync all manifests to root VERSION
  python3 scripts/sync-version.py 0.2.0        # set root VERSION to 0.2.0 and sync all
  python3 scripts/sync-version.py --check      # verify all manifests match root VERSION
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSION_FILE = ROOT / "VERSION"
ROOT_CARGO = ROOT / "Cargo.toml"
TAURI_CARGO = ROOT / "app/src-tauri/Cargo.toml"
TAURI_CONF = ROOT / "app/src-tauri/tauri.conf.json"
UI_PKG = ROOT / "app/ui/package.json"
PKGBUILD = ROOT / "app/packaging/arch/PKGBUILD"


def read_version() -> str:
    return VERSION_FILE.read_text(encoding="utf-8").strip().lstrip("vV")


def replace_first(text: str, pattern: str, repl: str, label: str) -> str:
    new_text, count = re.subn(pattern, repl, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise RuntimeError(f"Could not find version field in {label}")
    return new_text


def extract_first(text: str, pattern: str, label: str) -> str:
    m = re.search(pattern, text, flags=re.MULTILINE)
    if not m:
        raise RuntimeError(f"Could not read version field in {label}")
    return m.group(1)


def check_all(expected: str) -> int:
    mismatches = []
    checks = [
        ("Cargo.toml", extract_first(ROOT_CARGO.read_text("utf-8"), r'^version = "([^"]+)"', "Cargo.toml")),
        ("app/src-tauri/Cargo.toml", extract_first(TAURI_CARGO.read_text("utf-8"), r'^version = "([^"]+)"', "app/src-tauri/Cargo.toml")),
        ("app/src-tauri/tauri.conf.json", json.loads(TAURI_CONF.read_text("utf-8")).get("version", "")),
        ("app/ui/package.json", json.loads(UI_PKG.read_text("utf-8")).get("version", "")),
        ("app/packaging/arch/PKGBUILD", extract_first(PKGBUILD.read_text("utf-8"), r'^pkgver=([^\s]+)', "PKGBUILD")),
    ]
    for name, actual in checks:
        if actual != expected:
            mismatches.append((name, actual))

    if mismatches:
        print(f"Version mismatch (VERSION={expected}):", file=sys.stderr)
        for name, actual in mismatches:
            print(f"  - {name}: {actual}", file=sys.stderr)
        return 1

    print(f"All manifests match VERSION={expected}")
    return 0


def sync_all(version: str) -> None:
    VERSION_FILE.write_text(f"{version}\n", encoding="utf-8")

    ROOT_CARGO.write_text(
        replace_first(ROOT_CARGO.read_text("utf-8"), r'^version = "[^"]+"', f'version = "{version}"', "Cargo.toml"),
        encoding="utf-8",
    )
    TAURI_CARGO.write_text(
        replace_first(TAURI_CARGO.read_text("utf-8"), r'^version = "[^"]+"', f'version = "{version}"', "app/src-tauri/Cargo.toml"),
        encoding="utf-8",
    )

    tauri_cfg = json.loads(TAURI_CONF.read_text("utf-8"))
    tauri_cfg["version"] = version
    TAURI_CONF.write_text(json.dumps(tauri_cfg, indent=2) + "\n", encoding="utf-8")

    ui_pkg = json.loads(UI_PKG.read_text("utf-8"))
    ui_pkg["version"] = version
    UI_PKG.write_text(json.dumps(ui_pkg, indent=2) + "\n", encoding="utf-8")

    PKGBUILD.write_text(
        replace_first(PKGBUILD.read_text("utf-8"), r'^pkgver=[^\s]+', f"pkgver={version}", "PKGBUILD"),
        encoding="utf-8",
    )
    print(f"Synchronized all manifests to {version}")


def main() -> int:
    args = sys.argv[1:]
    if args == ["--check"]:
        return check_all(read_version())
    if len(args) == 1 and not args[0].startswith("-"):
        ver = args[0].strip().lstrip("vV")
        if not re.match(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$", ver):
            print(f"Invalid semver version: {ver}", file=sys.stderr)
            return 2
        sync_all(ver)
        return 0
    if not args:
        sync_all(read_version())
        return 0

    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
