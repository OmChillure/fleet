# Runner testing procedures

How to **run** the three layers required by
[`testing-requirements.md`](./testing-requirements.md). That file is the
catalog. This file is the operator procedure. Tests live next to the
code; do not add a second framework.

Fail closed. Live `:3030`/`:8080` is this tree only when **both**
`GET /health` `git_sha` **and** UI `/fleet-git-sha.txt` equal
`scripts/tree-id.sh`. Rebuild after edits
(`./scripts/run-every-test.sh --prepare-kind` and host `dx`) before
layer 2 or 3.

There is no GitLab runner. Keep `.gitlab-ci.yml` as `when: never`.

Do not put the catalog on a recurring writer loop. Harvest with
`scripts/harvest-runner-catalog.sh` (or the layer commands below).
A missing child id is not a relaunch.

---

## Layer 1 — unit / regular

From the repo root, after
`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`
once (no flags; `PROFILE` / `ENABLE_*` in `.env`):

```bash
just test
just test-integration   # needs just db-up; DATABASE_URL in .env
```

`just test` is `cargo test --workspace --lib --exclude fleet-ui --locked`
plus driver/host. It never talks to a browser. `fleet-runners-view`
(extracted UI helpers) is included in `--workspace --lib`.

`just test-integration` covers:

| Crate | What |
| --- | --- |
| `fleet-db` `tests/worker_hosts_repos.rs` | pair consume-once, revoke, heartbeat, assignable, reclaim |
| `fleet-db` `control_plane_runner_id` | plane id is stable and is never a paired host |
| `fleet-server` `tests/worker_hosts_http.rs` | create/list/pair/lease/PATCH/revoke; plane first; plane DELETE 400 |
| `fleet-orchestrator` `tests/assignable_runner.rs` | picker ignores cloud/partners; native-only |
| `fleet-orchestrator` `tests/host_update_tick.rs` | hold ready while updating |
| `fleet-orchestrator` `tests/plane_runner_tick.rs` | plane off → no local spawn |
| `sandbox-lab/tests/test_install_worker.py` | script syntax + detect + `FLEET_PAIR_TOKEN` |
| `fleet-runners-view` `placeholders.rs` | Cloud four cards + Coming soon; partners trust copy; disclaimer default-off; ack ≠ lend/assignable |

`fleet-ui` is WASM. Chrome is layer 2. Pure helpers run on the host in
`fleet-runners-view` (`just test`): `routes.rs` (accordion title + child
labels/paths; `/{ws}/runners` and `/{ws}/workers` → Self-hosted) and
`placeholders.rs` (Cloud / Partners). Do not add a Python clone of those
helpers.

---

## Layer 2 — browser e2e

Needs this tree on `:8080` + `:3030` and Playwright
(`ENABLE_BROWSER_E2E=1` in repo-root `.env`).

```bash
./scripts/human-test.sh launch
python3 -m unittest tests.browser.test_app_e2e -v
./scripts/human-test.sh              # person walks the product
./scripts/human-test.sh record runners workers cloud partners
```

Identity: `python3 tests/browser/stack_identity.py` must pass. Foreign
Kind extraPortMappings are a fail.

Catalog → test (layer 2):

| Behavior | Procedure |
| --- | --- |
| Nav + Cloud + Partners | `test_07`, `test_10`, `test_11`, `test_12`, `test_13`; `test_catalog_cloud.test_cloud_page_coming_soon` |
| Disclaimer ack persists; no lend enablement | `test_14o_partners_ack`; `test_catalog_partners.test_ack_persists_and_does_not_enable_lend` |
| Redirects | `test_08` (`/workers`), `test_09` (`/runners`) |
| Add / pair / Done / token once | `test_05_workers_add_shows_pair_token` |
| Copy Unix / Windows / token (HTTP LAN fallback) | `test_14h_copy_buttons_http_lan_fallback`; `test_catalog_copy_buttons.test_copy_unix_windows_token_incl_http_lan_fallback` |
| Loopback URL warning | `test_14i_loopback_warning`; `test_catalog_loopback_warning.test_warning_when_command_is_loopback` |
| Cancel discards pending | `test_15_cancel_discards_pending_worker` |
| API is not a runner; empty Self-hosted | `test_16_control_plane_card`; `test_14l_plane_sandboxed`; `test_catalog_plane_sandboxed.test_toggle_sandboxed_on_the_plane_card` |
| Apply changes appears when dirty | `test_17_apply_changes_when_dirty` |
| Apply PATCH toast + pending apply | `test_14m_apply_toast`; `test_catalog_apply_toast.test_apply_toast_and_pending_apply_until_ack` |
| Widgets lock while pending | `test_14n_widgets_lock`; `test_catalog_widgets_lock.test_widgets_lock_while_pending_until_ack` |
| Rename a card | `test_catalog_rename.test_rename_card_enter_saves_escape_cancels` |
| Extra pills: pending apply, outdated | `test_catalog_pills.test_pending_apply_until_ack_and_outdated` |
| Remove worker confirm / cancel | `test_catalog_remove.test_remove_worker_confirm_removes_cancel_aborts` |
| Show removed after revoke of a paired box | `test_14k_show_removed`; `test_catalog_show_removed.test_show_removed_after_revoke_of_a_paired_box` |
| Auto-update off + Update now | `test_14d_update_now`; `test_catalog_update_now.test_update_now_when_behind_becomes_update_requested` |
| Poll list ~10s | `test_catalog_poll.test_card_updates_after_pair_heartbeat` |
| Specs on a live card | `test_catalog_specs.test_specs_on_a_live_card` |
| Software line after pair; not on plane | `test_14g_software_line`; `test_catalog_software` |
| last seen relative (“just now” / “never”) | `test_14j_last_seen`; `test_catalog_last_seen.test_last_seen_never_then_just_now` |
| Package banner is Current fleet-host version / Current fleet-driver version | `test_05` must not say Current runner build |
| Logout (must stay last in unittest name order) | `test_18_logout_returns_to_login` |

Human-test steps: **Runners / Self-hosted**, **Workers**, **Cloud**,
**Partners**. Demo scenes: `runners`, `workers`, `cloud`, `partners`.

---

## Layer 3 — real-life guest e2e

Default **on** for every `PROFILE` (repo-root `.env` should have
`ENABLE_RUNNER_GUEST_E2E=1`). Explicit `0` still wins. Spawning
every OS needs images under `.tmp/vms/`. Missing guests when the flag
is on is a **fail**.

```bash
# in repo-root .env
ENABLE_RUNNER_GUEST_E2E=1
```

Then:

```bash
# stack must be this tree
./scripts/human-test.sh launch
python3 -m unittest \
  crates.fleet-sandbox-driver.sandbox-lab.tests.test_runner_guest_e2e \
  -v
```

From the lab directory (what `run-full-suite.sh` discovers):

```bash
cd crates/fleet-sandbox-driver/sandbox-lab
python3 -m unittest tests.test_runner_guest_e2e -v
```

If the flag is **off**, live guest methods skip. The catalog-contract
tests in that file **always run** (procedures exist, platforms listed,
flag documented).

If the flag is **on** and a guest, the stack, or this-tree identity is
missing, that platform test **fails**. There is no WARN-and-continue.

**Skipped for now (live):** `linux-aarch64`, `linux-aarch64-musl`,
`linux-armv7`, `linux-armv7-musl`, `freebsd-amd64`, `darwin-x86_64`,
`darwin-arm64`, `windows-arm64`. Catalog still lists them. Re-enable
in `test_runner_guest_e2e.py` `LIVE_SKIPPED`.

### Per-platform procedure (every guest)

Do this on each id in the matrix
(`linux-x86_64`, `linux-x86_64-musl`, `linux-x86_64-bookworm`,
`linux-aarch64`, `linux-aarch64-musl`, `linux-armv7`, `linux-armv7-musl`,
`freebsd-amd64`, `darwin-x86_64`, `darwin-arm64`, `windows-x86_64`,
`windows-arm64`):

1. **Identity.** `GET /health` `git_sha` and UI `/fleet-git-sha.txt`
   equal `scripts/tree-id.sh`. Served `manifest.json` for this platform
   is the package the guest must install.
2. **Guest up.** Boot the image for that id (see `.tmp/vms/<id>/` or the
   platform `BUILD.md`). SSH or serial console ready.
3. **Pair.** Owner **Add worker** (or `POST /api/workspaces/{id}/worker-hosts`).
   Copy the Unix or Windows command for this OS.
4. **Install.** Paste the command on the guest. It must install
   `fleet-host`, `fleet-driver`, pair, and start the
   daemon with no second command.
5. **Ping.** `GET …/worker-hosts` shows the box **online**, last seen
   recent, hostname/platform match, 8-hex id set, specs if the OS can
   read them (CPU, RAM, GPU/iGPU).
6. **Job.** Enable Native on the guest (default). Plane may stay on.
   Tick a ready task. Guest takes a **native** lease; job is not
   `runtime=container`.
7. **Hold.** **Stop taking new jobs** → **Apply changes**. Heartbeat
   acks (`pending apply` clears). Next tick does not lease this box.
   A job already running finishes.
8. **Cap.** Set max jobs to 1, Apply, ack. A second ready task does not
   start on this box.
9. **Update.** Serve a newer staged package. Auto-update on + idle →
   guest updates; software line **updating** then **up to date**.
   Auto-update off → **Update now** then one apply on the next
   heartbeat. Mid-swap (`software_updating`) + no other assignable
   host → task stays **ready** (no local spawn).
10. **Offline.** Stop `fleet-host`. After **90s** the card is **offline**.
    Start it again → **online**.
11. **Reclaim.** Remove worker. Re-run install (or pair) on the **same**
    guest. `machine.id` (or hostname+platform) reclaims the same 8-hex
    id. The old disabled clone is hidden.
12. **No host.** With **no** assignable guest, a ready task stays ready.
    The API does not local-spawn.
13. **Sandboxed.** Checking Sandboxed on the guest must **not** start a
    container/k3d job.
14. **SHA reuse.** Second install of the same package skips matching
    bins; a mismatched SHA refetches.

Windows guests use the PowerShell command. Unix guests use the `sh`
command. ARM64 Windows is detected from the machine, not from x64
PowerShell.

### Guest images

Do not invent a second install path. Use the existing per-OS lab under
`.tmp/vms/` and `scripts/stage-worker-dist.sh` /
`scripts/stage-worker-dist-docker.sh` so the guest pulls **this** tree’s
package.

---

## Commands (cheat sheet)

```bash
# 1. unit / regular
just test
just test-integration

# 2. e2e
ENABLE_BROWSER_E2E=1   # in .env
./scripts/human-test.sh launch
python3 -m unittest tests.browser.test_app_e2e -v

# 3. real-life
ENABLE_RUNNER_GUEST_E2E=1   # in .env
python3 -m unittest \
  crates.fleet-sandbox-driver.sandbox-lab.tests.test_runner_guest_e2e -v
```
