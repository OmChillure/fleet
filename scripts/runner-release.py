#!/usr/bin/env python3
"""Independent version × hash locks for fleet-host and fleet-driver.

Each bin has its own [release.BIN] version + parent. Parent = sha256 of
sorted \"platform=hex\\n\" lines for the shipped OS set.

  check     — fail if a table is inconsistent, or staged bins do not match
  register  — rewrite both [release.BIN] children + parents from a dist dir
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

SHIP_PLATFORMS = ("linux-x86_64", "linux-aarch64", "windows-x86_64")
BINS = ("fleet-host", "fleet-driver")


def parent_digest(children: dict[str, str]) -> str:
    lines = [f"{plat}={sha.lower()}\n" for plat, sha in sorted(children.items())]
    return hashlib.sha256("".join(lines).encode()).hexdigest()


def parse_bin_release(text: str, bin_name: str) -> tuple[str, str, dict[str, str]]:
    version = ""
    parent = ""
    children: dict[str, str] = {}
    section = ""
    want = f"release.{bin_name}"
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]")
            continue
        m = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"\s*$', line)
        if not m or section != want:
            continue
        key, val = m.group(1), m.group(2).strip()
        if key == "version":
            version = val
        elif key == "parent":
            parent = val.lower()
        elif key in SHIP_PLATFORMS:
            children[key] = val.lower()
    return version, parent, children


def parse_versions(text: str, section: str) -> dict[str, str]:
    out: dict[str, str] = {}
    current = ""
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            current = line.strip("[]")
            continue
        m = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"\s*$', line)
        if m and current == section:
            out[m.group(1)] = m.group(2).strip()
    return out


def bin_stem(name: str) -> str:
    return name[:-4] if name.endswith(".exe") else name


def hashes_from_dist(dest: Path) -> dict[str, dict[str, str]]:
    out: dict[str, dict[str, str]] = {b: {} for b in BINS}
    for plat in SHIP_PLATFORMS:
        man = dest / plat / "manifest.json"
        if not man.is_file():
            continue
        data = json.loads(man.read_text())
        for key, meta in (data.get("bins") or {}).items():
            stem = bin_stem(str(key))
            if stem not in BINS or not isinstance(meta, dict):
                continue
            sha = (meta.get("sha256") or "").strip().lower()
            if re.fullmatch(r"[0-9a-f]{64}", sha):
                out[stem][plat] = sha
    return out


def rewrite_releases(
    text: str,
    releases: dict[str, tuple[str, str, dict[str, str]]],
) -> str:
    head: list[str] = []
    in_release = False
    for raw in text.splitlines(keepends=True):
        stripped = raw.strip()
        if stripped.startswith("["):
            name = stripped.strip("[]")
            in_release = name == "release" or name.startswith("release.")
            if in_release:
                continue
        if in_release:
            continue
        head.append(raw)
    body = "".join(head).rstrip() + "\n"
    for bin_ in BINS:
        version, parent, children = releases[bin_]
        body += f"\n[release.{bin_}]\n"
        body += f"version = \"{version}\"\n"
        body += f"parent = \"{parent}\"\n"
        for plat in SHIP_PLATFORMS:
            body += f"{plat} = \"{children.get(plat, '')}\"\n"
    return body


def cmd_check(toml_path: Path, dest: Path | None, require_all: bool) -> int:
    text = toml_path.read_text()
    pkg = parse_versions(text, "versions")
    ver = parse_versions(text, "verify")
    errors: list[str] = []
    for bin_ in BINS:
        version, parent, children = parse_bin_release(text, bin_)
        if not version:
            errors.append(f"[release.{bin_}].version is missing")
        if pkg.get(bin_) and version and pkg[bin_] != version:
            errors.append(
                f"[release.{bin_}].version {version} != [versions] {bin_}={pkg[bin_]}"
            )
        if ver.get(bin_) and version and ver[bin_] != version:
            errors.append(
                f"[release.{bin_}].version {version} != [verify] {bin_}={ver[bin_]}"
            )
        miss = [p for p in SHIP_PLATFORMS if not re.fullmatch(r"[0-9a-f]{64}", children.get(p, ""))]
        if miss:
            errors.append(f"[release.{bin_}] missing 64-hex hashes: {', '.join(miss)}")
            continue
        got = parent_digest(children)
        if parent and parent != got:
            errors.append(f"[release.{bin_}].parent {parent} does not match children {got}")
        if dest is not None and dest.is_dir():
            staged = hashes_from_dist(dest).get(bin_, {})
            if require_all:
                for plat in SHIP_PLATFORMS:
                    if plat not in staged:
                        errors.append(f"staged {plat}/{bin_} missing under {dest}")
            for plat, sha in staged.items():
                want = children.get(plat, "")
                if not want:
                    errors.append(f"{plat}/{bin_} hash {sha} is not registered for {version}")
                elif sha != want:
                    errors.append(
                        f"{plat}/{bin_} hash changed ({sha}) but {bin_} {version} "
                        f"is still registered as {want}. Bump [versions]/{bin_} and "
                        f"[release.{bin_}].version, restage, then "
                        f"scripts/register-runner-release.sh"
                    )
    if errors:
        print("runner-release: FAIL", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1
    parts = []
    for bin_ in BINS:
        v, p, _ = parse_bin_release(text, bin_)
        parts.append(f"{bin_}={v}/{p[:12]}…")
    print("runner-release: ok " + " ".join(parts))
    return 0


def cmd_register(toml_path: Path, dest: Path) -> int:
    staged = hashes_from_dist(dest)
    miss = [f"{p}/{b}" for b in BINS for p in SHIP_PLATFORMS if p not in staged.get(b, {})]
    if miss:
        print("register: staged dist missing " + ", ".join(miss), file=sys.stderr)
        return 1
    text = toml_path.read_text()
    pkg = parse_versions(text, "versions")
    releases: dict[str, tuple[str, str, dict[str, str]]] = {}
    for bin_ in BINS:
        version = pkg.get(bin_) or ""
        if not version:
            print(f"register: [versions].{bin_} missing", file=sys.stderr)
            return 1
        children = staged[bin_]
        releases[bin_] = (version, parent_digest(children), children)
    toml_path.write_text(rewrite_releases(text, releases))
    for bin_, (version, parent, _) in releases.items():
        print(f"registered {bin_} {version} parent={parent}")
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
