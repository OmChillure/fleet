#!/usr/bin/env python3
"""Version × hash lock for shipped runner packages.

Parent = sha256 of sorted \"platform/bin=hex\\n\" lines (bin without .exe).
Children are per shipped OS (linux-x86_64, linux-aarch64, windows-x86_64).

  check     — fail if table is inconsistent, or staged bins do not match
  register  — rewrite [release] children + parent from a dist dir
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

SHIP_PLATFORMS = ("linux-x86_64", "linux-aarch64", "windows-x86_64")
BINS = ("fleet-host", "fleet-driver")


def parent_digest(children: dict[tuple[str, str], str]) -> str:
    lines = [f"{plat}/{bin_}={sha.lower()}\n" for (plat, bin_), sha in sorted(children.items())]
    return hashlib.sha256("".join(lines).encode()).hexdigest()


def parse_release(text: str) -> tuple[str, str, dict[tuple[str, str], str]]:
    version = ""
    parent = ""
    children: dict[tuple[str, str], str] = {}
    section = ""
    plat = ""
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]")
            plat = ""
            if section.startswith("release.") and section.count(".") == 1:
                plat = section.split(".", 1)[1]
            continue
        m = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"\s*$', line)
        if not m:
            continue
        key, val = m.group(1), m.group(2)
        if section == "release":
            if key == "version":
                version = val.strip()
            elif key == "parent":
                parent = val.strip().lower()
        elif plat:
            stem = key[:-4] if key.endswith(".exe") else key
            if stem in BINS:
                children[(plat, stem)] = val.strip().lower()
    return version, parent, children


def required_children(children: dict[tuple[str, str], str]) -> list[str]:
    missing = []
    for plat in SHIP_PLATFORMS:
        for bin_ in BINS:
            sha = children.get((plat, bin_), "")
            if not re.fullmatch(r"[0-9a-f]{64}", sha or ""):
                missing.append(f"{plat}/{bin_}")
    return missing


def bin_stem(name: str) -> str:
    return name[:-4] if name.endswith(".exe") else name


def hashes_from_dist(dest: Path) -> dict[tuple[str, str], str]:
    out: dict[tuple[str, str], str] = {}
    for plat in SHIP_PLATFORMS:
        man = dest / plat / "manifest.json"
        if not man.is_file():
            continue
        data = json.loads(man.read_text())
        bins = data.get("bins") or {}
        for key, meta in bins.items():
            stem = bin_stem(str(key))
            if stem not in BINS:
                continue
            if isinstance(meta, dict):
                sha = (meta.get("sha256") or "").strip().lower()
            else:
                sha = ""
            if re.fullmatch(r"[0-9a-f]{64}", sha):
                out[(plat, stem)] = sha
    return out


def rewrite_release(text: str, version: str, parent: str, children: dict[tuple[str, str], str]) -> str:
    head: list[str] = []
    in_release = False
    for raw in text.splitlines(keepends=True):
        stripped = raw.strip()
        if stripped.startswith("["):
            name = stripped.strip("[]")
            if name == "release" or name.startswith("release."):
                in_release = True
                continue
            in_release = False
        if in_release:
            continue
        head.append(raw)
    body = "".join(head).rstrip() + "\n\n"
    body += f"[release]\nversion = \"{version}\"\nparent = \"{parent}\"\n"
    for plat in SHIP_PLATFORMS:
        body += f"\n[release.{plat}]\n"
        for bin_ in BINS:
            sha = children.get((plat, bin_), "")
            body += f"{bin_} = \"{sha}\"\n"
    return body


def cmd_check(toml_path: Path, dest: Path | None, require_all: bool) -> int:
    text = toml_path.read_text()
    version, parent, children = parse_release(text)
    errors: list[str] = []
    if not version:
        errors.append("[release].version is missing")
    miss = required_children(children)
    if miss:
        errors.append("release table missing 64-hex hashes: " + ", ".join(miss))
    got_parent = parent_digest(children) if not miss else ""
    if parent and got_parent and parent != got_parent:
        errors.append(
            f"[release].parent {parent} does not match children {got_parent}"
        )
    if dest is not None and dest.is_dir():
        staged = hashes_from_dist(dest)
        if require_all:
            for plat in SHIP_PLATFORMS:
                for bin_ in BINS:
                    if (plat, bin_) not in staged:
                        errors.append(f"staged {plat}/{bin_} missing under {dest}")
        for key, sha in staged.items():
            want = children.get(key, "")
            if not want:
                errors.append(
                    f"{key[0]}/{key[1]} hash {sha} is not registered for {version}"
                )
            elif sha != want:
                errors.append(
                    f"{key[0]}/{key[1]} hash changed ({sha}) but version {version} "
                    f"is still registered as {want}. Bump [versions]/[verify]/[release].version "
                    f"and run scripts/register-runner-release.sh"
                )
    if errors:
        print("runner-release: FAIL", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1
    print(f"runner-release: ok version={version} parent={parent[:12]}…")
    return 0


def cmd_register(toml_path: Path, dest: Path) -> int:
    staged = hashes_from_dist(dest)
    miss = [f"{p}/{b}" for p in SHIP_PLATFORMS for b in BINS if (p, b) not in staged]
    if miss:
        print("register: staged dist missing " + ", ".join(miss), file=sys.stderr)
        return 1
    text = toml_path.read_text()
    version, _, _ = parse_release(text)
    if not version:
        print("register: [release].version missing — set it before registering", file=sys.stderr)
        return 1
    parent = parent_digest(staged)
    toml_path.write_text(rewrite_release(text, version, parent, staged))
    print(f"registered {version} parent={parent}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        print(__doc__)
        return 2
    cmd = argv[1]
    root = Path(__file__).resolve().parents[1]
    toml_path = root / "runner-packages.toml"
    require = "--require-all" in argv
    rest = [a for a in argv[2:] if a != "--require-all"]
    dest = Path(rest[0]) if rest else Path(
        __import__("os").environ.get("FLEET_WORKER_DIST_DIR", str(root / ".tmp/vms/dist"))
    )
    if cmd == "check":
        return cmd_check(toml_path, dest, require)
    if cmd == "register":
        return cmd_register(toml_path, dest)
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
