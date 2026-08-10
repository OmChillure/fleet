"""Resolve ENABLE_* the same way install-full-suite-deps.sh does.

Priority for a given key:
1. repo-root `.env` (sourced over the process environment)
2. process environment
3. PROFILE=full|lab|minimal defaults when the value is still empty
4. SKIP_KIND / SKIP_FULL_IMAGES / SKIP_HARNESS force the matching ENABLE_* off
"""
from __future__ import annotations

import os
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]

ENABLE_NAMES = (
    "ENABLE_SANDBOX_LAB",
    "ENABLE_HARNESS_OPENCODE",
    "ENABLE_HARNESS_PI",
    "ENABLE_HARNESS_CODEX",
    "ENABLE_HARNESS_GROK",
    "ENABLE_FULL_IMAGES",
    "ENABLE_KIND",
    "ENABLE_OLLAMA",
    "ENABLE_LMSTUDIO_PROXY",
    "ENABLE_GITSSH",
    "ENABLE_BROWSER_E2E",
)

# Mirrors install-full-suite-deps.sh `case "${PROFILE}"`.
PROFILE_DEFAULTS: dict[str, dict[str, bool]] = {
    "full": {
        "ENABLE_SANDBOX_LAB": True,
        "ENABLE_HARNESS_OPENCODE": True,
        "ENABLE_HARNESS_PI": True,
        "ENABLE_HARNESS_CODEX": True,
        "ENABLE_HARNESS_GROK": True,
        "ENABLE_FULL_IMAGES": True,
        "ENABLE_KIND": True,
        "ENABLE_OLLAMA": True,
        "ENABLE_LMSTUDIO_PROXY": True,
        "ENABLE_GITSSH": True,
        "ENABLE_BROWSER_E2E": True,
    },
    "lab": {
        "ENABLE_SANDBOX_LAB": True,
        "ENABLE_HARNESS_OPENCODE": True,
        "ENABLE_HARNESS_PI": True,
        "ENABLE_HARNESS_CODEX": False,
        "ENABLE_HARNESS_GROK": False,
        "ENABLE_FULL_IMAGES": False,
        "ENABLE_KIND": False,
        "ENABLE_OLLAMA": True,
        "ENABLE_LMSTUDIO_PROXY": True,
        "ENABLE_GITSSH": True,
        "ENABLE_BROWSER_E2E": False,
    },
    "minimal": {
        "ENABLE_SANDBOX_LAB": True,
        "ENABLE_HARNESS_OPENCODE": True,
        "ENABLE_HARNESS_PI": False,
        "ENABLE_HARNESS_CODEX": False,
        "ENABLE_HARNESS_GROK": False,
        "ENABLE_FULL_IMAGES": False,
        "ENABLE_KIND": False,
        "ENABLE_OLLAMA": True,
        "ENABLE_LMSTUDIO_PROXY": False,
        "ENABLE_GITSSH": False,
        "ENABLE_BROWSER_E2E": False,
    },
}


def _truthy(val: str) -> bool:
    return val.strip().strip("'\"").lower() in ("1", "true", "yes")


def parse_dotenv(text: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        key, val = s.split("=", 1)
        out[key.strip()] = val.strip().strip("'\"")
    return out


def resolve_flags(
    *,
    environ: dict[str, str] | None = None,
    dotenv_path: Path | None = None,
    dotenv_text: str | None = None,
) -> dict[str, bool]:
    if dotenv_text is None:
        path = dotenv_path if dotenv_path is not None else REPO / ".env"
        dotenv_text = path.read_text(encoding="utf-8") if path.is_file() else ""
    file_vars = parse_dotenv(dotenv_text)
    env = os.environ if environ is None else environ

    def raw(name: str) -> str | None:
        if name in file_vars:
            return file_vars[name]
        if name in env:
            return env[name]
        return None

    profile = (raw("PROFILE") or "full").strip() or "full"
    defaults = PROFILE_DEFAULTS.get(profile, {})

    flags: dict[str, bool] = {}
    for name in ENABLE_NAMES:
        val = raw(name)
        if val is None or val.strip() == "":
            flags[name] = bool(defaults.get(name, False))
        else:
            flags[name] = _truthy(val)

    if raw("ENABLE_SANDBOX_LAB") is None and raw("ENABLE_LAB") is not None:
        flags["ENABLE_SANDBOX_LAB"] = _truthy(raw("ENABLE_LAB") or "0")

    if _truthy(raw("SKIP_KIND") or "0"):
        flags["ENABLE_KIND"] = False
    if _truthy(raw("SKIP_FULL_IMAGES") or "0"):
        flags["ENABLE_FULL_IMAGES"] = False
    if _truthy(raw("SKIP_HARNESS") or "0"):
        flags["ENABLE_HARNESS_OPENCODE"] = False
        flags["ENABLE_HARNESS_PI"] = False
        flags["ENABLE_HARNESS_CODEX"] = False
        flags["ENABLE_HARNESS_GROK"] = False
    return flags


def env_flag(
    name: str,
    *,
    environ: dict[str, str] | None = None,
    dotenv_path: Path | None = None,
    dotenv_text: str | None = None,
) -> bool:
    return resolve_flags(
        environ=environ, dotenv_path=dotenv_path, dotenv_text=dotenv_text
    ).get(name, False)
