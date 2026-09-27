#!/usr/bin/env python3
"""Independent version × hash locks for fleet-host and fleet-driver.

Each bin has a line version (0.0.3) and a per-OS rev (0, 1, 2, …).
Windows-only bytes become 0.0.3.1; Linux stays 0.0.3. A line bump to
0.0.4 resets every OS rev to 0.

  check           — fail if a table is inconsistent, or staged bins do not match
  register        — rewrite children, revs, parents from a dist dir
                    --keep-author-revs keeps the toml rev and fails if a
                    hash changed vs --baseline without a rev bump
  platform-ok     — exit 0 if DIST/PLATFORM hashes match [release]
  display-version — print line or line.rev for BIN PLATFORM
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

SHIP_PLATFORMS = ("linux-x86_64", "linux-aarch64", "windows-x86_64")
BINS = ("fleet-host", "fleet-driver")


def parent_digest(children: dict[str, str]) -> str:
    lines = [f"{plat}={sha.lower()}\n" for plat, sha in sorted(children.items())]
    return hashlib.sha256("".join(lines).encode()).hexdigest()


@dataclass
class BinRelease:
    version: str = ""
    parent: str = ""
    children: dict[str, str] = field(default_factory=dict)
    revs: dict[str, int] = field(default_factory=dict)


def display_version(line: str, rev: int) -> str:
    if rev > 0:
        return f"{line}.{rev}"
    return line


def parse_bin_release(text: str, bin_name: str) -> BinRelease:
    out = BinRelease()
    section = ""
    want = f"release.{bin_name}"
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]")
            continue
        if section != want:
            continue
        mq = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"\s*$', line)
        mi = re.match(r'^([A-Za-z0-9_-]+)\s*=\s*(\d+)\s*$', line)
        if mq:
            key, val = mq.group(1), mq.group(2).strip()
            if key == "version":
                out.version = val
            elif key == "parent":
                out.parent = val.lower()
            elif key in SHIP_PLATFORMS:
                out.children[key] = val.lower()
            elif key.endswith("-rev") and key[: -len("-rev")] in SHIP_PLATFORMS:
                try:
                    out.revs[key[: -len("-rev")]] = int(val)
                except ValueError:
                    pass
        elif mi:
            key, val = mi.group(1), int(mi.group(2))
            if key.endswith("-rev") and key[: -len("-rev")] in SHIP_PLATFORMS:
                out.revs[key[: -len("-rev")]] = val
    return out


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


def rewrite_releases(text: str, releases: dict[str, BinRelease]) -> str:
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
        rel = releases[bin_]
        body += f"\n[release.{bin_}]\n"
        body += f"version = \"{rel.version}\"\n"
        body += f"parent = \"{rel.parent}\"\n"
        for plat in SHIP_PLATFORMS:
            body += f"{plat} = \"{rel.children.get(plat, '')}\"\n"
            body += f"{plat}-rev = {int(rel.revs.get(plat, 0))}\n"
    return body


def cmd_check(toml_path: Path, dest: Path | None, require_all: bool) -> int:
    text = toml_path.read_text()
    pkg = parse_versions(text, "versions")
    ver = parse_versions(text, "verify")
    errors: list[str] = []
    for bin_ in BINS:
        rel = parse_bin_release(text, bin_)
        if not rel.version:
            errors.append(f"[release.{bin_}].version is missing")
        if pkg.get(bin_) and rel.version and pkg[bin_] != rel.version:
            errors.append(
                f"[release.{bin_}].version {rel.version} != [versions] {bin_}={pkg[bin_]}"
            )
        if ver.get(bin_) and rel.version and ver[bin_] != rel.version:
            errors.append(
                f"[release.{bin_}].version {rel.version} != [verify] {bin_}={ver[bin_]}"
            )
        miss = [p for p in SHIP_PLATFORMS if not re.fullmatch(r"[0-9a-f]{64}", rel.children.get(p, ""))]
        if miss:
            errors.append(f"[release.{bin_}] missing 64-hex hashes: {', '.join(miss)}")
            continue
        got = parent_digest(rel.children)
        if rel.parent and rel.parent != got:
            errors.append(f"[release.{bin_}].parent {rel.parent} does not match children {got}")
        if dest is not None and dest.is_dir():
            staged = hashes_from_dist(dest).get(bin_, {})
            if require_all:
                for plat in SHIP_PLATFORMS:
                    if plat not in staged:
                        errors.append(f"staged {plat}/{bin_} missing under {dest}")
            for plat, sha in staged.items():
                want = rel.children.get(plat, "")
                if not want:
                    errors.append(f"{plat}/{bin_} hash {sha} is not registered for {rel.version}")
                elif sha != want:
                    shown = display_version(rel.version, rel.revs.get(plat, 0))
                    errors.append(
                        f"{plat}/{bin_} hash changed ({sha}) but {shown} is still "
                        f"registered as {want}. The author bumps "
                        f"[release.{bin_}].{plat}-rev (or the line, which resets "
                        f"every OS rev to 0). The deployer restages and registers "
                        f"the hash with --keep-author-revs."
                    )
    if errors:
        print("runner-release: FAIL", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1
    parts = []
    for bin_ in BINS:
        rel = parse_bin_release(text, bin_)
        parts.append(f"{bin_}={rel.version}/{rel.parent[:12]}…")
    print("runner-release: ok " + " ".join(parts))
    return 0


def cmd_platform_ok(toml_path: Path, dest: Path, plat: str) -> int:
    """Exit 0 when both bins for plat are staged and match [release]."""
    if plat not in SHIP_PLATFORMS:
        print(f"platform-ok: unknown platform {plat}", file=sys.stderr)
        return 2
    text = toml_path.read_text()
    staged = hashes_from_dist(dest)
    for bin_ in BINS:
        rel = parse_bin_release(text, bin_)
        want = rel.children.get(plat, "")
        got = staged.get(bin_, {}).get(plat, "")
        if not re.fullmatch(r"[0-9a-f]{64}", want or ""):
            print(f"platform-ok: [release.{bin_}].{plat} missing", file=sys.stderr)
            return 1
        if got != want:
            return 1
    return 0


def _staged_complete(staged: dict[str, dict[str, str]]) -> list[str]:
    return [f"{p}/{b}" for b in BINS for p in SHIP_PLATFORMS if p not in staged.get(b, {})]


def cmd_register(
    toml_path: Path,
    dest: Path,
    *,
    keep_author_revs: bool = False,
    baseline: Path | None = None,
) -> int:
    """Rewrite [release] hashes from staged bins.

    Default bumps a per-OS rev when that OS hash changes. ``keep_author_revs``
    writes the staged hashes at the rev already in ``toml_path`` and fails
    when a hash differs from ``baseline`` (the previous commit's toml) but
    that rev did not increase. A line bump still requires every rev to be 0.
    """
    staged = hashes_from_dist(dest)
    miss = _staged_complete(staged)
    if miss:
        print("register: staged dist missing " + ", ".join(miss), file=sys.stderr)
        return 1
    text = toml_path.read_text()
    pkg = parse_versions(text, "versions")
    base_text = baseline.read_text() if baseline is not None and baseline.is_file() else text
    releases: dict[str, BinRelease] = {}
    errors: list[str] = []
    for bin_ in BINS:
        version = pkg.get(bin_) or ""
        if not version:
            print(f"register: [versions].{bin_} missing", file=sys.stderr)
            return 1
        author = parse_bin_release(text, bin_)
        prev = parse_bin_release(base_text, bin_)
        line_bumped = bool(prev.version) and prev.version != version
        revs: dict[str, int] = {}
        for plat in SHIP_PLATFORMS:
            sha = staged[bin_].get(plat, "")
            prev_sha = prev.children.get(plat, "")
            prev_rev = int(prev.revs.get(plat, 0))
            author_rev = int(author.revs.get(plat, 0))
            if not keep_author_revs:
                if line_bumped:
                    revs[plat] = 0
                elif sha and prev_sha and sha != prev_sha:
                    revs[plat] = prev_rev + 1
                else:
                    revs[plat] = author_rev
                continue
            if line_bumped:
                if author_rev != 0:
                    errors.append(
                        f"{plat}/{bin_}: line bumped to {version} but {plat}-rev is "
                        f"{author_rev}; a line bump resets every OS rev to 0"
                    )
                revs[plat] = 0
            elif not prev_sha or sha == prev_sha:
                revs[plat] = author_rev
            elif author_rev > prev_rev:
                revs[plat] = author_rev
            else:
                shown = display_version(version, author_rev)
                errors.append(
                    f"{plat}/{bin_} hash changed but {shown} did not bump "
                    f"{plat}-rev (still {author_rev}, previous {prev_rev}). "
                    f"Bump that rev, or bump the line (resets every OS rev to 0). "
                    f"The deployer writes the hash; it does not invent the rev."
                )
                revs[plat] = author_rev
        releases[bin_] = BinRelease(
            version=version,
            parent=parent_digest(staged[bin_]),
            children=dict(staged[bin_]),
            revs=revs,
        )
    if errors:
        print("runner-release: FAIL", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        return 1
    toml_path.write_text(rewrite_releases(text, releases))
    for bin_, rel in releases.items():
        bits = [display_version(rel.version, rel.revs.get(p, 0)) + f"@{p}" for p in SHIP_PLATFORMS]
        print(f"registered {bin_} line={rel.version} parent={rel.parent} " + " ".join(bits))
    return 0


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        print(__doc__)
        return 2
    cmd = argv[1]
    root = Path(__file__).resolve().parents[1]
    toml_path = root / "runner-packages.toml"
    require = "--require-all" in argv
    keep_revs = "--keep-author-revs" in argv
    baseline: Path | None = None
    rest: list[str] = []
    skip_next = False
    for i, a in enumerate(argv[2:]):
        if skip_next:
            skip_next = False
            continue
        if a in ("--require-all", "--keep-author-revs"):
            continue
        if a == "--baseline":
            if i + 1 >= len(argv[2:]):
                print("usage: --baseline TOML", file=sys.stderr)
                return 2
            baseline = Path(argv[2:][i + 1])
            skip_next = True
            continue
        rest.append(a)
    dest = Path(rest[0]) if rest else Path(
        __import__("os").environ.get("FLEET_WORKER_DIST_DIR", str(root / ".tmp/vms/dist"))
    )
    if cmd == "check":
        return cmd_check(toml_path, dest, require)
    if cmd == "register":
        return cmd_register(
            toml_path, dest, keep_author_revs=keep_revs, baseline=baseline
        )
    if cmd == "platform-ok":
        if len(rest) < 2:
            print("usage: runner-release.py platform-ok DIST PLATFORM", file=sys.stderr)
            return 2
        return cmd_platform_ok(toml_path, Path(rest[0]), rest[1])
    if cmd == "display-version":
        if len(rest) < 2:
            print("usage: runner-release.py display-version BIN PLATFORM", file=sys.stderr)
            return 2
        rel = parse_bin_release(toml_path.read_text(), rest[0])
        sys.stdout.write(display_version(rel.version, rel.revs.get(rest[1], 0)))
        return 0
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
