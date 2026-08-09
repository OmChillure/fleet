# Agent notes

Install is one command, no arguments:

`crates/fleet-driver/sandbox-lab/scripts/install-full-suite-deps.sh`

Read that script for OS/RAM/disk/`PROFILE`/`ENABLE_*`. Persist those in the
repo-root `.env` (same file as DATABASE_URL / JWT / OAuth). Do not invent a
second install guide and do not pass flags on the command line.

Before you commit: check whether the install script needs updating (new deps,
flags, OS/kind/lab behavior) and whether any tests must be added. No
regressions — run the relevant suite (`run-full-suite.sh` / cargo / lab
unittests) and do not leave install or CI worse than you found it.
