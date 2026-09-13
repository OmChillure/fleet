# QA test matrix

Inventory of **every automated test function** in this tree against the
criteria in [`qa-rulebook.md`](./qa-rulebook.md).

**Evaluation has not been done.** Every criteria cell is `—` (not yet
judged). Fill later with `yes` / `no` / `n/a`. Do not treat a blank
matrix as a pass.

How to read a later fill:

| Mark | Meaning |
| --- | --- |
| `—` | Not evaluated yet |
| `yes` | Meets the criterion |
| `no` | Fails the criterion |
| `n/a` | Criterion does not apply to this layer |

## Criteria columns (from the rulebook laws)

| Col | Law | Question to answer later |
| --- | --- | --- |
| L1 | Live contract | Does this lock a user-noticeable contract, not a tautology / copy / fiction? |
| L2 | Fail closed | Missing required dep is a fail, never a skip / warn / `\|\| true`? |
| L3 | This tree | Live/e2e identity is this `tree-id`, after a rebuild? |
| L4 | Isolated + deterministic | Alone, any order, same result? Unique fixtures? |
| L5 | Public surface | Tests behavior through the public API/UI, not private wiring? |
| L6 | Right layer | Cheapest layer that can prove the claim? Language of the SUT? |
| L7 | Human still required | If user/operator visible, is walk + film still in scope? (usually `n/a` for units) |

Must-rules `QA-R1`–`QA-R36` are the detailed form of these laws.
Evaluate against the laws first; drop to a rule id in Notes when a `no`
needs a citation.

## Inventory rules

Counted on this worktree (`main` at generation time):

- Rust: every `#[test]` / `#[tokio::test]` function.
- Python: every `def test_*` / `async def test_*`.
- Plus the named `case(...)` checks in `tests/browser/test_integrations_live.py`
  (script runner, not unittest).
- Not counted: helpers (`common.rs`, `*_helpers.py`, `lab_env.py`,
  `record_demo.py`, `surface_scope.py`, `stack_identity.py`), fixtures,
  human-test pause steps, film scenes.

`Gate` is a fact about how the test is selected, not a quality judgment:

| Gate | Meaning |
| --- | --- |
| `default` | Runs in the normal suite for its layer |
| `ignored` | `#[ignore]` — opt-in (`--ignored`) |
| `live-skipped` | Guest OS listed in `LIVE_SKIPPED` |
| `live-flag` | Needs `FLEET_LIVE_INTEGRATIONS=1` |

`Layer` is inferred from path, not evaluated:

| Layer | Where |
| --- | --- |
| `unit` | `#[cfg(test)]` next to Rust code |
| `unit-ui-host` | `fleet-ui` host-side tests (excluded from `just test --workspace --lib`) |
| `rust-integration` | `crates/<pkg>/tests/*.rs` |
| `lab` | `sandbox-lab/tests/` |
| `lab-workspace` | `sandbox-lab/workspace/tests/` |
| `guest-e2e` | `test_runner_guest_e2e.py` |
| `browser-e2e` | Playwright / browser e2e |
| `source-contract` | Browser-tree contracts that do not need a live stack |
| `live-integrations` | Live OAuth / Grok cases |

## Counts

Total rows: **1499**

| Layer | Count |
| --- | ---: |
| `unit` | 774 |
| `rust-integration` | 356 |
| `lab` | 87 |
| `source-contract` | 73 |
| `browser-e2e` | 72 |
| `unit-ui-host` | 67 |
| `guest-e2e` | 24 |
| `lab-workspace` | 24 |
| `live-integrations` | 22 |

| Gate | Count |
| --- | ---: |
| `default` | 1462 |
| `live-flag` | 22 |
| `live-skipped` | 8 |
| `ignored` | 7 |

| Suite | Count |
| --- | ---: |
| `agent-fleet-contracts` | 18 |
| `fleet-auth` | 68 |
| `fleet-bus` | 13 |
| `fleet-chat-md` | 5 |
| `fleet-db` | 125 |
| `fleet-driver` | 26 |
| `fleet-host` | 128 |
| `fleet-integrations` | 88 |
| `fleet-operator` | 9 |
| `fleet-orchestrator` | 219 |
| `fleet-runners-view` | 88 |
| `fleet-sandbox-driver` | 61 |
| `fleet-server` | 282 |
| `fleet-ui` | 67 |
| `sandbox-lab` | 111 |
| `sandbox-lab/workspace` | 24 |
| `tests/browser` | 167 |

## Suites

- [`agent-fleet-contracts`](#agent-fleet-contracts) (18)
- [`fleet-auth`](#fleet-auth) (68)
- [`fleet-bus`](#fleet-bus) (13)
- [`fleet-chat-md`](#fleet-chat-md) (5)
- [`fleet-db`](#fleet-db) (125)
- [`fleet-driver`](#fleet-driver) (26)
- [`fleet-host`](#fleet-host) (128)
- [`fleet-integrations`](#fleet-integrations) (88)
- [`fleet-operator`](#fleet-operator) (9)
- [`fleet-orchestrator`](#fleet-orchestrator) (219)
- [`fleet-runners-view`](#fleet-runners-view) (88)
- [`fleet-sandbox-driver`](#fleet-sandbox-driver) (61)
- [`fleet-server`](#fleet-server) (282)
- [`fleet-ui`](#fleet-ui) (67)
- [`sandbox-lab`](#sandbox-lab) (111)
- [`sandbox-lab/workspace`](#sandbox-labworkspace) (24)
- [`tests/browser`](#testsbrowser) (167)

## agent-fleet-contracts

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 140 | `order_is_process_lt_container_lt_vm` | — | — | — | — | — | — | — | |
| 2 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 146 | `k8s_is_not_an_isolation_class` | — | — | — | — | — | — | — | |
| 3 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 154 | `pick_cheapest_that_meets_need` | — | — | — | — | — | — | — | |
| 4 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 175 | `pick_does_not_fall_back_below_need` | — | — | — | — | — | — | — | |
| 5 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 182 | `kube_pod_is_container_kubevirt_is_vm` | — | — | — | — | — | — | — | |
| 6 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 189 | `loopback_includes_127_slash_8_and_ipv6` | — | — | — | — | — | — | — | |
| 7 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 206 | `k8s_guest_callback_url_must_not_be_loopback` | — | — | — | — | — | — | — | |
| 8 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 265 | `strip_drops_host_absolute_bins_not_relative` | — | — | — | — | — | — | — | |
| 9 | `unit` | `default` | `crates/agent-fleet-contracts/src/isolation.rs` | 285 | `strip_isolated_guest_env_drops_host_path_bins_and_loopback_secrets` | — | — | — | — | — | — | — | |
| 10 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 66 | `sandbox_failed_roundtrip_matches_catalogue_shape` | — | — | — | — | — | — | — | |
| 11 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 118 | `bus_message_new_merges_sandbox_scope` | — | — | — | — | — | — | — | |
| 12 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 136 | `bus_message_new_merges_session_and_turn` | — | — | — | — | — | — | — | |
| 13 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 156 | `agent_event_stream_chunk` | — | — | — | — | — | — | — | |
| 14 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 181 | `cmd_ack_roundtrip` | — | — | — | — | — | — | — | |
| 15 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 194 | `producer_integration_prefix` | — | — | — | — | — | — | — | |
| 16 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 203 | `producer_unknown_label_preserved` | — | — | — | — | — | — | — | |
| 17 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 210 | `sandbox_backend_unknown_roundtrip` | — | — | — | — | — | — | — | |
| 18 | `unit` | `default` | `crates/agent-fleet-contracts/src/lib.rs` | 221 | `create_sandbox_command_json` | — | — | — | — | — | — | — | |

## fleet-auth

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 19 | `unit` | `default` | `crates/fleet-auth/src/config.rs` | 105 | `jwt_secret_shorter_than_32_fails_from_env` | — | — | — | — | — | — | — | |
| 20 | `unit` | `default` | `crates/fleet-auth/src/config.rs` | 117 | `valid_secret_from_env_works` | — | — | — | — | — | — | — | |
| 21 | `unit` | `default` | `crates/fleet-auth/src/config.rs` | 126 | `auth_config_constructs_in_struct` | — | — | — | — | — | — | — | |
| 22 | `unit` | `default` | `crates/fleet-auth/src/config.rs` | 141 | `from_env_reads_signup_token` | — | — | — | — | — | — | — | |
| 23 | `unit` | `default` | `crates/fleet-auth/src/cookies.rs` | 81 | `set_auth_cookies_http_only_path_and_secure_follows_config` | — | — | — | — | — | — | — | |
| 24 | `unit` | `default` | `crates/fleet-auth/src/cookies.rs` | 114 | `clear_auth_cookies_max_age_zero` | — | — | — | — | — | — | — | |
| 25 | `unit` | `default` | `crates/fleet-auth/src/password.rs` | 35 | `hash_and_verify_ok` | — | — | — | — | — | — | — | |
| 26 | `unit` | `default` | `crates/fleet-auth/src/password.rs` | 41 | `wrong_password_returns_false` | — | — | — | — | — | — | — | |
| 27 | `unit` | `default` | `crates/fleet-auth/src/password.rs` | 47 | `garbage_hash_is_error_not_panic` | — | — | — | — | — | — | — | |
| 28 | `unit` | `default` | `crates/fleet-auth/src/rate_limit.rs` | 139 | `prefers_x_real_ip_over_spoofed_xff` | — | — | — | — | — | — | — | |
| 29 | `unit` | `default` | `crates/fleet-auth/src/rate_limit.rs` | 148 | `xff_uses_rightmost_valid_hop` | — | — | — | — | — | — | — | |
| 30 | `unit` | `default` | `crates/fleet-auth/src/rate_limit.rs` | 154 | `invalid_xff_token_falls_through_to_x_real_ip` | — | — | — | — | — | — | — | |
| 31 | `unit` | `default` | `crates/fleet-auth/src/rate_limit.rs` | 163 | `connect_info_used_when_proxy_headers_absent` | — | — | — | — | — | — | — | |
| 32 | `unit` | `default` | `crates/fleet-auth/src/rate_limit.rs` | 173 | `evicts_empty_windows` | — | — | — | — | — | — | — | |
| 33 | `unit` | `default` | `crates/fleet-auth/src/routes/auth.rs` | 473 | `signup_token_unset_is_open` | — | — | — | — | — | — | — | |
| 34 | `unit` | `default` | `crates/fleet-auth/src/routes/auth.rs` | 480 | `signup_token_required_when_configured` | — | — | — | — | — | — | — | |
| 35 | `unit` | `default` | `crates/fleet-auth/src/routes/members.rs` | 362 | `can_manage_owner_and_admin_true_member_false` | — | — | — | — | — | — | — | |
| 36 | `unit` | `default` | `crates/fleet-auth/src/routes/members.rs` | 560 | `patch_or_delete_member_in_another_workspace_is_403` | — | — | — | — | — | — | — | |
| 37 | `unit` | `default` | `crates/fleet-auth/src/routes/personas.rs` | 436 | `slugify_basic` | — | — | — | — | — | — | — | |
| 38 | `unit` | `default` | `crates/fleet-auth/src/routes/personas.rs` | 443 | `starter_catalog_has_eight_roles` | — | — | — | — | — | — | — | |
| 39 | `unit` | `default` | `crates/fleet-auth/src/routes/personas.rs` | 645 | `unknown_persona_id_is_rejected_on_assign_and_not_persisted` | — | — | — | — | — | — | — | |
| 40 | `unit` | `default` | `crates/fleet-auth/src/routes/personas.rs` | 849 | `personas_page_does_not_call_llm` | — | — | — | — | — | — | — | |
| 41 | `unit` | `default` | `crates/fleet-auth/src/routes/workspaces.rs` | 218 | `recreate_slug_is_deterministic` | — | — | — | — | — | — | — | |
| 42 | `unit` | `default` | `crates/fleet-auth/src/tokens.rs` | 92 | `issue_and_decode_access_jwt` | — | — | — | — | — | — | — | |
| 43 | `unit` | `default` | `crates/fleet-auth/src/tokens.rs` | 111 | `wrong_secret_fails` | — | — | — | — | — | — | — | |
| 44 | `unit` | `default` | `crates/fleet-auth/src/tokens.rs` | 129 | `expired_token_fails` | — | — | — | — | — | — | — | |
| 45 | `unit` | `default` | `crates/fleet-auth/src/tokens.rs` | 155 | `logout_decode_accepts_expired_token` | — | — | — | — | — | — | — | |
| 46 | `unit` | `default` | `crates/fleet-auth/src/tokens.rs` | 179 | `logout_decode_rejects_wrong_secret` | — | — | — | — | — | — | — | |
| 47 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 37 | `signup_201_no_session_cookies` | — | — | — | — | — | — | — | |
| 48 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 65 | `signup_rejects_bad_email` | — | — | — | — | — | — | — | |
| 49 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 73 | `signup_rejects_short_password` | — | — | — | — | — | — | — | |
| 50 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 93 | `signup_rejects_long_password` | — | — | — | — | — | — | — | |
| 51 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 101 | `signup_duplicate_email_409` | — | — | — | — | — | — | — | |
| 52 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 134 | `login_sets_httponly_cookies_and_workspace` | — | — | — | — | — | — | — | |
| 53 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 165 | `login_unknown_and_bad_password_same_message` | — | — | — | — | — | — | — | |
| 54 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 201 | `concurrent_logins_two_sessions` | — | — | — | — | — | — | — | |
| 55 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 226 | `refresh_rotates_cookies` | — | — | — | — | — | — | — | |
| 56 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 246 | `refresh_reuse_revokes_family` | — | — | — | — | — | — | — | |
| 57 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 281 | `logout_then_me_401` | — | — | — | — | — | — | — | |
| 58 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 293 | `me_lists_at_least_one_workspace` | — | — | — | — | — | — | — | |
| 59 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 308 | `create_workspace_slug_and_owner` | — | — | — | — | — | — | — | |
| 60 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 333 | `create_workspace_empty_and_too_long_400` | — | — | — | — | — | — | — | |
| 61 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 364 | `other_user_forbidden_on_personas` | — | — | — | — | — | — | — | |
| 62 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 384 | `unauthenticated_cookie_routes_are_401` | — | — | — | — | — | — | — | |
| 63 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 402 | `refresh_without_cookie_is_401` | — | — | — | — | — | — | — | |
| 64 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 410 | `logout_clears_cookies_and_blocks_refresh` | — | — | — | — | — | — | — | |
| 65 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 447 | `login_is_case_insensitive_on_email` | — | — | — | — | — | — | — | |
| 66 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 460 | `signup_display_name_sets_workspace_name` | — | — | — | — | — | — | — | |
| 67 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 474 | `inactive_user_cannot_login` | — | — | — | — | — | — | — | |
| 68 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 494 | `create_workspace_then_list_includes_both` | — | — | — | — | — | — | — | |
| 69 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 526 | `invite_adds_existing_user_and_member_cannot_invite` | — | — | — | — | — | — | — | |
| 70 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 589 | `pending_invite_joins_on_signup` | — | — | — | — | — | — | — | |
| 71 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 634 | `login_accepts_pending_invites` | — | — | — | — | — | — | — | |
| 72 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 722 | `expired_invite_does_not_join_on_signup` | — | — | — | — | — | — | — | |
| 73 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 768 | `last_owner_cannot_patch_self_to_member` | — | — | — | — | — | — | — | |
| 74 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 808 | `last_owner_cannot_delete_self` | — | — | — | — | — | — | — | |
| 75 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 848 | `member_cannot_list_or_revoke_invites` | — | — | — | — | — | — | — | |
| 76 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 921 | `revoke_invite_then_signup_does_not_join` | — | — | — | — | — | — | — | |
| 77 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 968 | `invite_existing_member_is_already_a_member` | — | — | — | — | — | — | — | |
| 78 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1003 | `invite_role_owner_is_rejected` | — | — | — | — | — | — | — | |
| 79 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1026 | `owner_get_invites_omits_token` | — | — | — | — | — | — | — | |
| 80 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1072 | `admin_cannot_invite_admin_can_invite_member` | — | — | — | — | — | — | — | |
| 81 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1144 | `admin_cannot_delete_owner_or_patch_roles` | — | — | — | — | — | — | — | |
| 82 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1255 | `login_rate_limited_by_forwarded_ip` | — | — | — | — | — | — | — | |
| 83 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1295 | `login_rate_limit_uses_real_ip_not_spoofed_xff` | — | — | — | — | — | — | — | |
| 84 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1338 | `signup_open_when_token_unset` | — | — | — | — | — | — | — | |
| 85 | `rust-integration` | `default` | `crates/fleet-auth/tests/auth_http.rs` | 1345 | `signup_requires_token_when_configured` | — | — | — | — | — | — | — | |
| 86 | `rust-integration` | `default` | `crates/fleet-auth/tests/personas_no_llm.rs` | 86 | `persona_seed_create_patch_archive_do_not_invoke_llm` | — | — | — | — | — | — | — | |

## fleet-bus

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 87 | `unit` | `default` | `crates/fleet-bus/src/codec.rs` | 54 | `sandbox_failed_json_roundtrip` | — | — | — | — | — | — | — | |
| 88 | `unit` | `default` | `crates/fleet-bus/src/codec.rs` | 65 | `string_roundtrip_matches_bytes` | — | — | — | — | — | — | — | |
| 89 | `unit` | `default` | `crates/fleet-bus/src/codec.rs` | 73 | `wire_shape_has_type_and_payload` | — | — | — | — | — | — | — | |
| 90 | `unit` | `default` | `crates/fleet-bus/src/codec.rs` | 85 | `cmd_ack_roundtrip` | — | — | — | — | — | — | — | |
| 91 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 104 | `sandbox_event_matches_architecture_examples` | — | — | — | — | — | — | — | |
| 92 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 116 | `liveliness_driver_and_host` | — | — | — | — | — | — | — | |
| 93 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 128 | `event_type_chunk_strips_domain_prefix` | — | — | — | — | — | — | — | |
| 94 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 135 | `key_for_message_session` | — | — | — | — | — | — | — | |
| 95 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 154 | `key_for_message_agent_event` | — | — | — | — | — | — | — | |
| 96 | `unit` | `default` | `crates/fleet-bus/src/keys.rs` | 178 | `session_and_cmd_keys` | — | — | — | — | — | — | — | |
| 97 | `unit` | `default` | `crates/fleet-bus/src/lib.rs` | 37 | `sandbox_failed_key_and_json_roundtrip` | — | — | — | — | — | — | — | |
| 98 | `unit` | `default` | `crates/fleet-bus/src/session.rs` | 74 | `client_config_embeds_locator` | — | — | — | — | — | — | — | |
| 99 | `unit` | `default` | `crates/fleet-bus/src/session.rs` | 88 | `live_connect_skips_without_zenohd` | — | — | — | — | — | — | — | |

## fleet-chat-md

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | `unit` | `default` | `crates/fleet-chat-md/src/lib.rs` | 503 | `renders_headings_lists_and_code` | — | — | — | — | — | — | — | |
| 101 | `unit` | `default` | `crates/fleet-chat-md/src/lib.rs` | 516 | `strips_raw_html_and_bad_links` | — | — | — | — | — | — | — | |
| 102 | `unit` | `default` | `crates/fleet-chat-md/src/lib.rs` | 524 | `does_not_emit_raw_html_from_user_text` | — | — | — | — | — | — | — | |
| 103 | `unit` | `default` | `crates/fleet-chat-md/src/lib.rs` | 574 | `parts_split_code_blocks` | — | — | — | — | — | — | — | |
| 104 | `unit` | `default` | `crates/fleet-chat-md/src/lib.rs` | 589 | `json_highlight_strings_and_nums` | — | — | — | — | — | — | — | |

## fleet-db

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 105 | `unit` | `default` | `crates/fleet-db/src/models/gpu.rs` | 296 | `x1_comet_lake_is_uhd` | — | — | — | — | — | — | — | |
| 106 | `unit` | `default` | `crates/fleet-db/src/models/gpu.rs` | 304 | `intel_generations` | — | — | — | — | — | — | — | |
| 107 | `unit` | `default` | `crates/fleet-db/src/models/gpu.rs` | 332 | `amd_apple_qualcomm` | — | — | — | — | — | — | — | |
| 108 | `unit` | `default` | `crates/fleet-db/src/models/gpu.rs` | 356 | `normalize_strips_trademarks` | — | — | — | — | — | — | — | |
| 109 | `unit` | `default` | `crates/fleet-db/src/models/gpu.rs` | 373 | `discrete_xeon_has_no_igpu` | — | — | — | — | — | — | — | |
| 110 | `unit` | `default` | `crates/fleet-db/src/models/plane_runner.rs` | 24 | `plane_id_is_stable_per_workspace` | — | — | — | — | — | — | — | |
| 111 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 773 | `stale_online_host_is_reported_offline` | — | — | — | — | — | — | — | |
| 112 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 793 | `public_hardware_infers_intel_uhd_for_x1_cpu` | — | — | — | — | — | — | — | |
| 113 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 806 | `cluster_label_is_trimmed_case_insensitive` | — | — | — | — | — | — | — | |
| 114 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 838 | `v1_lease_gate_is_host_or_cluster_container` | — | — | — | — | — | — | — | |
| 115 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 863 | `isolation_parse_and_public_view` | — | — | — | — | — | — | — | |
| 116 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 885 | `pending_and_disabled_are_not_rewritten_to_offline` | — | — | — | — | — | — | — | |
| 117 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 894 | `public_json_never_includes_hashes_or_tokens` | — | — | — | — | — | — | — | |
| 118 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 967 | `software_status_unknown_without_report_or_manifest` | — | — | — | — | — | — | — | |
| 119 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 981 | `software_status_behind_when_present_bin_mismatches` | — | — | — | — | — | — | — | |
| 120 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 997 | `missing_fleet_driver_is_behind` | — | — | — | — | — | — | — | |
| 121 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1013 | `reporting_only_legacy_fleet_worker_is_not_current` | — | — | — | — | — | — | — | |
| 122 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1030 | `software_status_current_when_bins_match` | — | — | — | — | — | — | — | |
| 123 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1046 | `software_status_uses_package_version_then_hash` | — | — | — | — | — | — | — | |
| 124 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1084 | `software_status_behind_on_mixed_bins` | — | — | — | — | — | — | — | |
| 125 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1100 | `software_status_maps_windows_exe_keys` | — | — | — | — | — | — | — | |
| 126 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1138 | `software_status_requires_both_bins_not_bake_id_alone` | — | — | — | — | — | — | — | |
| 127 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1154 | `software_status_falls_back_to_git_sha_when_package_has_no_bins` | — | — | — | — | — | — | — | |
| 128 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1179 | `compute_software_status_never_treats_file_hash_as_bake_id` | — | — | — | — | — | — | — | |
| 129 | `unit` | `default` | `crates/fleet-db/src/models/worker_host.rs` | 1306 | `with_desired_sets_outdated_only_when_behind` | — | — | — | — | — | — | — | |
| 130 | `unit` | `default` | `crates/fleet-db/src/models/workspace.rs` | 93 | `missing_timestamp_is_not_accepted` | — | — | — | — | — | — | — | |
| 131 | `unit` | `default` | `crates/fleet-db/src/models/workspace.rs` | 100 | `recorded_timestamp_is_accepted` | — | — | — | — | — | — | — | |
| 132 | `unit` | `default` | `crates/fleet-db/src/models/workspace.rs` | 110 | `ack_serializes_timestamp_only` | — | — | — | — | — | — | — | |
| 133 | `unit` | `default` | `crates/fleet-db/src/models/workspace.rs` | 136 | `parse_accepts_canonical_strings_only` | — | — | — | — | — | — | — | |
| 134 | `unit` | `default` | `crates/fleet-db/src/pool.rs` | 201 | `supabase_transaction_pooler_becomes_session` | — | — | — | — | — | — | — | |
| 135 | `unit` | `default` | `crates/fleet-db/src/pool.rs` | 211 | `hosted_url_keeps_existing_sslmode` | — | — | — | — | — | — | — | |
| 136 | `unit` | `default` | `crates/fleet-db/src/pool.rs` | 220 | `local_url_untouched` | — | — | — | — | — | — | — | |
| 137 | `unit` | `default` | `crates/fleet-db/src/pool.rs` | 226 | `neon_gets_sslmode` | — | — | — | — | — | — | — | |
| 138 | `unit` | `default` | `crates/fleet-db/src/repos/cases.rs` | 374 | `process_due_claim_due_statuses_exclude_completed_cancelled_failed` | — | — | — | — | — | — | — | |
| 139 | `unit` | `default` | `crates/fleet-db/src/repos/cases.rs` | 397 | `process_due_claim_due_sql_skips_terminal_statuses` | — | — | — | — | — | — | — | |
| 140 | `unit` | `default` | `crates/fleet-db/src/repos/cases.rs` | 431 | `decide_on_terminal_apply_patch_sql_refuses_reopen` | — | — | — | — | — | — | — | |
| 141 | `unit` | `default` | `crates/fleet-db/src/repos/integrations.rs` | 378 | `patch_agent_defaults_sql_updates_connected_only_never_inserts` | — | — | — | — | — | — | — | |
| 142 | `unit` | `default` | `crates/fleet-db/src/repos/worker_hosts.rs` | 1130 | `pair_and_machine_token_prefixes_and_hash_roundtrip` | — | — | — | — | — | — | — | |
| 143 | `unit` | `default` | `crates/fleet-db/src/repos/worker_hosts.rs` | 1147 | `hash_token_is_not_plaintext` | — | — | — | — | — | — | — | |
| 144 | `rust-integration` | `default` | `crates/fleet-db/tests/cases_repos.rs` | 99 | `reclaim_stale_deciding_returns_stuck_deciding_to_waiting` | — | — | — | — | — | — | — | |
| 145 | `rust-integration` | `default` | `crates/fleet-db/tests/cases_repos.rs` | 169 | `reclaim_stale_deciding_does_not_revive_terminal_cases` | — | — | — | — | — | — | — | |
| 146 | `rust-integration` | `default` | `crates/fleet-db/tests/cases_repos.rs` | 327 | `process_due_claim_due_skips_completed_and_cancelled_with_due_wake` | — | — | — | — | — | — | — | |
| 147 | `rust-integration` | `default` | `crates/fleet-db/tests/cases_repos.rs` | 433 | `process_due_reclaim_then_claim_due_does_not_revive_terminals` | — | — | — | — | — | — | — | |
| 148 | `rust-integration` | `default` | `crates/fleet-db/tests/cases_repos.rs` | 536 | `decide_on_terminal_apply_patch_is_conflict_and_does_not_reopen` | — | — | — | — | — | — | — | |
| 149 | `rust-integration` | `default` | `crates/fleet-db/tests/control_plane_events_repos.rs` | 30 | `insert_list_and_mark_published` | — | — | — | — | — | — | — | |
| 150 | `rust-integration` | `default` | `crates/fleet-db/tests/control_plane_events_repos.rs` | 106 | `duplicate_message_id_is_conflict` | — | — | — | — | — | — | — | |
| 151 | `rust-integration` | `default` | `crates/fleet-db/tests/control_plane_events_repos.rs` | 137 | `list_unpublished_respects_limit` | — | — | — | — | — | — | — | |
| 152 | `rust-integration` | `default` | `crates/fleet-db/tests/control_plane_events_repos.rs` | 175 | `list_and_get_never_return_another_workspace_events` | — | — | — | — | — | — | — | |
| 153 | `rust-integration` | `default` | `crates/fleet-db/tests/integrations_repos.rs` | 83 | `disconnect_drops_credentials_other_workspace_same_provider_intact` | — | — | — | — | — | — | — | |
| 154 | `rust-integration` | `default` | `crates/fleet-db/tests/integrations_repos.rs` | 204 | `consume_oauth_state_nonce_is_once_across_workspace_and_survives_disconnect` | — | — | — | — | — | — | — | |
| 155 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 72 | `list_pending_invites_for_email_skips_expired` | — | — | — | — | — | — | — | |
| 156 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 106 | `list_open_invites_excludes_expired` | — | — | — | — | — | — | — | |
| 157 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 135 | `accept_invite_and_pending_join_refuse_expired` | — | — | — | — | — | — | — | |
| 158 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 166 | `revoke_invite_drops_from_open_and_pending_second_revoke_false` | — | — | — | — | — | — | — | |
| 159 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 200 | `expired_invite_does_not_block_reinvite_same_email` | — | — | — | — | — | — | — | |
| 160 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 237 | `insert_invite_unexpired_same_email_is_conflict` | — | — | — | — | — | — | — | |
| 161 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 302 | `accept_pending_invites_joins_every_multiple_pending_workspace` | — | — | — | — | — | — | — | |
| 162 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 497 | `list_pending_invites_for_email_returns_multiple_pending_workspaces` | — | — | — | — | — | — | — | |
| 163 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 545 | `accept_pending_invites_multiple_pending_continues_if_already_member` | — | — | — | — | — | — | — | |
| 164 | `rust-integration` | `default` | `crates/fleet-db/tests/invites_repos.rs` | 598 | `expired_invite_cannot_be_accepted_signup_does_not_join_that_workspace` | — | — | — | — | — | — | — | |
| 165 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 57 | `insert_tx_duplicate_email_normalized_is_conflict_with_exact_message` | — | — | — | — | — | — | — | |
| 166 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 116 | `workspace_slug_unique_is_conflict` | — | — | — | — | — | — | — | |
| 167 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 152 | `session_insert_and_find_by_refresh_hash` | — | — | — | — | — | — | — | |
| 168 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 200 | `chat_conversation_scoped_to_user_and_workspace` | — | — | — | — | — | — | — | |
| 169 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 251 | `from_sqlx_maps_23505_to_conflict_and_is_unique_violation` | — | — | — | — | — | — | — | |
| 170 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 279 | `live_unique_violation_through_map_query_err_is_unique` | — | — | — | — | — | — | — | |
| 171 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 308 | `ticket_monitor_and_inbox_upsert_and_launch` | — | — | — | — | — | — | — | |
| 172 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 405 | `claim_launch_blocks_a_second_claim` | — | — | — | — | — | — | — | |
| 173 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 443 | `ticket_reopen_after_failed_run_and_mark_done` | — | — | — | — | — | — | — | |
| 174 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 511 | `case_create_claim_and_complete` | — | — | — | — | — | — | — | |
| 175 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 604 | `partners_disclaimer_defaults_null_and_first_write_wins` | — | — | — | — | — | — | — | |
| 176 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 656 | `accept_partners_disclaimer_does_not_change_worker_hosts_or_assignable` | — | — | — | — | — | — | — | |
| 177 | `rust-integration` | `default` | `crates/fleet-db/tests/repos_integration.rs` | 751 | `workspace_min_isolation_defaults_process_and_stores_wire_strings` | — | — | — | — | — | — | — | |
| 178 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 463 | `pair_token_has_required_prefix_and_entropy` | — | — | — | — | — | — | — | |
| 179 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 475 | `create_pending_then_list_for_workspace` | — | — | — | — | — | — | — | |
| 180 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 523 | `pair_token_works_once_second_consume_fails` | — | — | — | — | — | — | — | |
| 181 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 584 | `pair_token_one_hour_ttl_cannot_be_reused` | — | — | — | — | — | — | — | |
| 182 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 727 | `pair_token_consume_once_and_expired_must_fail` | — | — | — | — | — | — | — | |
| 183 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1086 | `re_pair_same_machine_id_reclaims_existing_row` | — | — | — | — | — | — | — | |
| 184 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1312 | `re_pair_same_machine_id_reclaims_not_hostname_collision` | — | — | — | — | — | — | — | |
| 185 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1394 | `re_pair_same_machine_id_does_not_reclaim_other_workspace` | — | — | — | — | — | — | — | |
| 186 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1459 | `re_pair_same_hostname_without_machine_id_reclaims_existing_row` | — | — | — | — | — | — | — | |
| 187 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1488 | `re_pair_same_hostname_without_machine_id_does_not_steal_live_row` | — | — | — | — | — | — | — | |
| 188 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1524 | `re_pair_same_hostname_without_machine_id_reclaims_stale_row` | — | — | — | — | — | — | — | |
| 189 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1564 | `two_machine_ids_same_hostname_stay_two_rows` | — | — | — | — | — | — | — | |
| 190 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1613 | `patch_bumps_settings_gen_until_ack` | — | — | — | — | — | — | — | |
| 191 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1651 | `each_settings_apply_increments_settings_gen` | — | — | — | — | — | — | — | |
| 192 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1740 | `ack_behind_settings_gen_stays_pending` | — | — | — | — | — | — | — | |
| 193 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1868 | `heartbeat_ack_old_settings_gen_stays_pending` | — | — | — | — | — | — | — | |
| 194 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 1970 | `patch_name_persists_and_bumps_settings_gen` | — | — | — | — | — | — | — | |
| 195 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2029 | `request_update_sets_timestamp_without_bumping_gen` | — | — | — | — | — | — | — | |
| 196 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2115 | `expired_pair_token_fails` | — | — | — | — | — | — | — | |
| 197 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2155 | `revoke_rejects_authenticate` | — | — | — | — | — | — | — | |
| 198 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2186 | `revoked_host_is_not_list_assignable_and_cannot_take_a_lease` | — | — | — | — | — | — | — | |
| 199 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2372 | `heartbeat_updates_last_seen_at` | — | — | — | — | — | — | — | |
| 200 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2532 | `heartbeat_with_machine_persists_agent_runtimes` | — | — | — | — | — | — | — | |
| 201 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2642 | `heartbeat_with_machine_none_does_not_wipe_agent_runtimes` | — | — | — | — | — | — | — | |
| 202 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2721 | `heartbeat_persists_hardware_inventory_empty_gpu_name_dropped` | — | — | — | — | — | — | — | |
| 203 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 2984 | `list_assignable_excludes_paused_disabled_stale` | — | — | — | — | — | — | — | |
| 204 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3069 | `list_assignable_skips_holding_new_jobs_even_with_running_job` | — | — | — | — | — | — | — | |
| 205 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3236 | `list_assignable_includes_cluster_host_from_other_workspace` | — | — | — | — | — | — | — | |
| 206 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3262 | `list_cluster_and_assignable_match_cluster_label_case` | — | — | — | — | — | — | — | |
| 207 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3307 | `create_pending_and_patch_normalize_cluster_label` | — | — | — | — | — | — | — | |
| 208 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3361 | `clearing_inferred_cluster_label_removes_cross_workspace_assignability` | — | — | — | — | — | — | — | |
| 209 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3468 | `cluster_label_only_from_bootstrap_null_created_by_migration_023` | — | — | — | — | — | — | — | |
| 210 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3696 | `list_assignable_excludes_sandboxed_isolation` | — | — | — | — | — | — | — | |
| 211 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3765 | `list_updating_recent_is_paired_updating_and_fresh` | — | — | — | — | — | — | — | |
| 212 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3886 | `claim_next_spawn_for_host_is_scoped_to_host_and_kind` | — | — | — | — | — | — | — | |
| 213 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3941 | `claim_next_spawn_for_host_in_workspace_does_not_lease_foreign_workspace` | — | — | — | — | — | — | — | |
| 214 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 3976 | `dead_for_session_voids_ready_and_leased_spawn_claims` | — | — | — | — | — | — | — | |
| 215 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4045 | `complete_owned_rejects_wrong_host` | — | — | — | — | — | — | — | |
| 216 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4091 | `complete_owned_uses_claim_workspace_not_host_home` | — | — | — | — | — | — | — | |
| 217 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4117 | `pair_token_hashed_at_rest_list_get_never_return_plaintext` | — | — | — | — | — | — | — | |
| 218 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4163 | `replace_pair_token_only_while_pending_and_invalidates_old` | — | — | — | — | — | — | — | |
| 219 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4213 | `stored_online_with_stale_last_seen_serializes_offline` | — | — | — | — | — | — | — | |
| 220 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4251 | `patch_install_model_persists_install_request_on_paired_host` | — | — | — | — | — | — | — | |
| 221 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4300 | `clear_install_request_if_other_spec_does_not_clear` | — | — | — | — | — | — | — | |
| 222 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4351 | `clear_install_request_if_matching_spec_clears` | — | — | — | — | — | — | — | |
| 223 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4393 | `patch_runtime_settings_merges_and_null_drops_key` | — | — | — | — | — | — | — | |
| 224 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4456 | `set_runtime_reported_does_not_change_settings_ack_gen` | — | — | — | — | — | — | — | |
| 225 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4506 | `patch_max_concurrent_0_and_9999_clamp_or_reject` | — | — | — | — | — | — | — | |
| 226 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4609 | `effective_max_concurrent_is_min_of_plane_and_advertised` | — | — | — | — | — | — | — | |
| 227 | `rust-integration` | `default` | `crates/fleet-db/tests/worker_hosts_repos.rs` | 4783 | `list_assignable_skips_host_at_max_concurrent_cap` | — | — | — | — | — | — | — | |
| 228 | `rust-integration` | `default` | `crates/fleet-db/tests/workforce_repos.rs` | 40 | `goal_and_approval_round_trip` | — | — | — | — | — | — | — | |
| 229 | `rust-integration` | `default` | `crates/fleet-db/tests/workforce_repos.rs` | 102 | `pick_goal_for_new_run_does_not_steal` | — | — | — | — | — | — | — | |

## fleet-driver

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 230 | `unit` | `default` | `crates/fleet-driver/src/acp.rs` | 917 | `pick_cached_token` | — | — | — | — | — | — | — | |
| 231 | `unit` | `default` | `crates/fleet-driver/src/acp.rs` | 928 | `pick_auth_none` | — | — | — | — | — | — | — | |
| 232 | `unit` | `default` | `crates/fleet-driver/src/acp.rs` | 934 | `format_message_chunk` | — | — | — | — | — | — | — | |
| 233 | `unit` | `default` | `crates/fleet-driver/src/acp.rs` | 943 | `blocked_stop_is_not_success` | — | — | — | — | — | — | — | |
| 234 | `unit` | `default` | `crates/fleet-driver/src/acp.rs` | 953 | `known_fail_and_success_stops` | — | — | — | — | — | — | — | |
| 235 | `unit` | `default` | `crates/fleet-driver/src/config.rs` | 139 | `auto_approve_defaults_true` | — | — | — | — | — | — | — | |
| 236 | `unit` | `default` | `crates/fleet-driver/src/harness.rs` | 128 | `grok_argv_with_model_and_approve` | — | — | — | — | — | — | — | |
| 237 | `unit` | `default` | `crates/fleet-driver/src/harness.rs` | 157 | `grok_argv_no_approve` | — | — | — | — | — | — | — | |
| 238 | `unit` | `default` | `crates/fleet-driver/src/harness.rs` | 172 | `override_cmd` | — | — | — | — | — | — | — | |
| 239 | `unit` | `default` | `crates/fleet-driver/src/harness.rs` | 188 | `unknown_harness_errs` | — | — | — | — | — | — | — | |
| 240 | `unit` | `default` | `crates/fleet-driver/src/prompt.rs` | 105 | `compose_includes_title_spec_acceptance` | — | — | — | — | — | — | — | |
| 241 | `unit` | `default` | `crates/fleet-driver/src/prompt.rs` | 122 | `load_prefers_prompt_md` | — | — | — | — | — | — | — | |
| 242 | `unit` | `default` | `crates/fleet-driver/src/prompt.rs` | 130 | `load_falls_back_to_spec` | — | — | — | — | — | — | — | |
| 243 | `unit` | `default` | `crates/fleet-driver/src/provision.rs` | 219 | `clones_local_file_url` | — | — | — | — | — | — | — | |
| 244 | `unit` | `default` | `crates/fleet-driver/src/provision.rs` | 237 | `workspace_json_roundtrip` | — | — | — | — | — | — | — | |
| 245 | `unit` | `default` | `crates/fleet-driver/src/provision.rs` | 250 | `missing_workspace_json_is_none` | — | — | — | — | — | — | — | |
| 246 | `unit` | `default` | `crates/fleet-driver/src/report.rs` | 43 | `write_artifacts_makes_result_md_long_enough_for_validate_done` | — | — | — | — | — | — | — | |
| 247 | `unit` | `default` | `crates/fleet-driver/src/terminal.rs` | 311 | `shell_line_detect` | — | — | — | — | — | — | — | |
| 248 | `rust-integration` | `ignored` | `crates/fleet-driver/tests/acp_grok.rs` | 15 | `grok_tiny_prompt_over_acp` | — | — | — | — | — | — | — | |
| 249 | `rust-integration` | `default` | `crates/fleet-driver/tests/acp_mock.rs` | 38 | `mock_happy_path_transcript` | — | — | — | — | — | — | — | |
| 250 | `rust-integration` | `default` | `crates/fleet-driver/tests/acp_mock.rs` | 56 | `mock_fail_is_not_ok` | — | — | — | — | — | — | — | |
| 251 | `rust-integration` | `default` | `crates/fleet-driver/tests/acp_mock.rs` | 74 | `mock_timeout` | — | — | — | — | — | — | — | |
| 252 | `rust-integration` | `default` | `crates/fleet-driver/tests/acp_mock.rs` | 94 | `mock_permission_auto_approve` | — | — | — | — | — | — | — | |
| 253 | `rust-integration` | `default` | `crates/fleet-driver/tests/acp_mock.rs` | 107 | `mock_terminal_create_echo` | — | — | — | — | — | — | — | |
| 254 | `rust-integration` | `default` | `crates/fleet-driver/tests/worker_protocol.rs` | 56 | `driver_binary_completes_mock_turn` | — | — | — | — | — | — | — | |
| 255 | `rust-integration` | `default` | `crates/fleet-driver/tests/worker_protocol.rs` | 152 | `orch_client_sends_bearer_and_maps_401_to_err` | — | — | — | — | — | — | — | |

## fleet-host

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 256 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 636 | `grok_missing_file_is_logged_out` | — | — | — | — | — | — | — | |
| 257 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 641 | `grok_fresh_session` | — | — | — | — | — | — | — | |
| 258 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 660 | `grok_expired_session` | — | — | — | — | — | — | — | |
| 259 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 679 | `parse_grok_models_lists_46_first` | — | — | — | — | — | — | — | |
| 260 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 688 | `parse_grok_cache_reads_xhigh` | — | — | — | — | — | — | — | |
| 261 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 719 | `parse_cli_model_ids_skips_headings` | — | — | — | — | — | — | — | |
| 262 | `unit` | `default` | `crates/fleet-host/src/agents.rs` | 729 | `sanitize_strips_secrets` | — | — | — | — | — | — | — | |
| 263 | `unit` | `default` | `crates/fleet-host/src/chat_cli.rs` | 248 | `grok_argv_includes_model_and_effort` | — | — | — | — | — | — | — | |
| 264 | `unit` | `default` | `crates/fleet-host/src/chat_cli.rs` | 259 | `claude_argv_is_print_mode_without_tools` | — | — | — | — | — | — | — | |
| 265 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 659 | `heartbeat_parses_desired_and_auto_update` | — | — | — | — | — | — | — | |
| 266 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 681 | `heartbeat_without_desired_is_null` | — | — | — | — | — | — | — | |
| 267 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 690 | `isolation_parse_maps_to_runtime` | — | — | — | — | — | — | — | |
| 268 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 739 | `isolation_parse_rejects_k8s_placement` | — | — | — | — | — | — | — | |
| 269 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 748 | `payload_isolation_overrides_runtime` | — | — | — | — | — | — | — | |
| 270 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 769 | `parse_sha256_sidecar_takes_first_token` | — | — | — | — | — | — | — | |
| 271 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 795 | `parse_sha256_sidecar_rejects_bake_id` | — | — | — | — | — | — | — | |
| 272 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 957 | `sidecar_match_reuses_bins_mismatch_refetches` | — | — | — | — | — | — | — | |
| 273 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 1024 | `fetch_install_sha256_sidecar_match_reuses_bins` | — | — | — | — | — | — | — | |
| 274 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 1072 | `fetch_install_sha256_sidecar_mismatch_refetches` | — | — | — | — | — | — | — | |
| 275 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 1115 | `fetch_install_sha256_invalid_sidecar_fails_closed` | — | — | — | — | — | — | — | |
| 276 | `unit` | `default` | `crates/fleet-host/src/client.rs` | 1176 | `fetch_install_sha256_missing_sidecar_fails_closed` | — | — | — | — | — | — | — | |
| 277 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 588 | `config_toml_roundtrip` | — | — | — | — | — | — | — | |
| 278 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 606 | `docker_network_defaults_to_bridge` | — | — | — | — | — | — | — | |
| 279 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 614 | `auto_update_defaults_true_and_can_be_disabled` | — | — | — | — | — | — | — | |
| 280 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 627 | `extra_env_cannot_replace_fleet_worker_token` | — | — | — | — | — | — | — | |
| 281 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 654 | `write_then_read_machine_token_roundtrip_trims` | — | — | — | — | — | — | — | |
| 282 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 669 | `read_missing_or_blank_machine_token_is_none` | — | — | — | — | — | — | — | |
| 283 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 677 | `delete_machine_token_removes_only_the_token_file` | — | — | — | — | — | — | — | |
| 284 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 692 | `save_does_not_copy_machine_token_into_config_toml` | — | — | — | — | — | — | — | |
| 285 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 715 | `local_config_debug_omits_machine_token` | — | — | — | — | — | — | — | |
| 286 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 733 | `machine_token_debug_and_display_omit_secret` | — | — | — | — | — | — | — | |
| 287 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 756 | `write_file_mode_sets_owner_only` | — | — | — | — | — | — | — | |
| 288 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 767 | `write_machine_token_creates_file_mode_0600_and_dir_0700` | — | — | — | — | — | — | — | |
| 289 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 786 | `write_machine_token_repairs_world_readable_mode` | — | — | — | — | — | — | — | |
| 290 | `unit` | `default` | `crates/fleet-host/src/config.rs` | 809 | `read_machine_token_repairs_world_readable_mode` | — | — | — | — | — | — | — | |
| 291 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 466 | `cr_name_is_session_dns_label` | — | — | — | — | — | — | — | |
| 292 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 475 | `from_cfg_guest_is_fleet_driver_without_harness` | — | — | — | — | — | — | — | |
| 293 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 494 | `spec_maps_identity_env_ttl_and_strips_non_fleet` | — | — | — | — | — | — | — | |
| 294 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 524 | `spec_allowlists_host_ip_callback` | — | — | — | — | — | — | — | |
| 295 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 535 | `spec_rejects_loopback_callback` | — | — | — | — | — | — | — | |
| 296 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 546 | `spec_rejects_oversized_md` | — | — | — | — | — | — | — | |
| 297 | `unit` | `default` | `crates/fleet-host/src/exec_k8s.rs` | 556 | `spec_requires_orchestrator_url` | — | — | — | — | — | — | — | |
| 298 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1604 | `parse_linux_meminfo` | — | — | — | — | — | — | — | |
| 299 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1611 | `parse_cpuinfo_prefers_model_name` | — | — | — | — | — | — | — | |
| 300 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1626 | `parse_cpuinfo_arm_hardware` | — | — | — | — | — | — | — | |
| 301 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1640 | `proc_stat_delta_is_used_over_total` | — | — | — | — | — | — | — | |
| 302 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1650 | `proc_stat_includes_iowait_as_idle` | — | — | — | — | — | — | — | |
| 303 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1657 | `nvidia_smi_csv_parses_mib` | — | — | — | — | — | — | — | |
| 304 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1669 | `nvidia_smi_csv_parses_utilization` | — | — | — | — | — | — | — | |
| 305 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1679 | `amdgpu_busy_percent_parses` | — | — | — | — | — | — | — | |
| 306 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1687 | `nvidia_smi_csv_strips_units_and_quotes` | — | — | — | — | — | — | — | |
| 307 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1696 | `dmidecode_memory_prefers_configured_speed` | — | — | — | — | — | — | — | |
| 308 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1717 | `smbios_type17_speed_and_ddr4` | — | — | — | — | — | — | — | |
| 309 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1735 | `lspci_extracts_marketing_name` | — | — | — | — | — | — | — | |
| 310 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1748 | `lspci_recognizes_intel_iris_xe` | — | — | — | — | — | — | — | |
| 311 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1758 | `lspci_recognizes_intel_uhd` | — | — | — | — | — | — | — | |
| 312 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1768 | `infers_x1_comet_lake_uhd` | — | — | — | — | — | — | — | |
| 313 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1776 | `infers_old_and_new_intel_igpu` | — | — | — | — | — | — | — | |
| 314 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1788 | `wmic_parses_intel_uhd` | — | — | — | — | — | — | — | |
| 315 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1797 | `macos_memory_and_display_parsers` | — | — | — | — | — | — | — | |
| 316 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1810 | `wmic_memory_and_gpu_parsers` | — | — | — | — | — | — | — | |
| 317 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1831 | `heartbeat_gpu_list_drops_empty_names_keeps_igpu` | — | — | — | — | — | — | — | |
| 318 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1857 | `gpu_json_normalize_does_not_invent_vram` | — | — | — | — | — | — | — | |
| 319 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1891 | `nvidia_smi_csv_drops_empty_gpu_names_keeps_igpu` | — | — | — | — | — | — | — | |
| 320 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1910 | `lspci_drops_empty_gpu_names_keeps_intel_igpu` | — | — | — | — | — | — | — | |
| 321 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1924 | `wmic_drops_empty_gpu_names_keeps_intel_igpu` | — | — | — | — | — | — | — | |
| 322 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1934 | `macos_displays_drops_empty_gpu_names_keeps_igpu` | — | — | — | — | — | — | — | |
| 323 | `unit` | `default` | `crates/fleet-host/src/hardware.rs` | 1944 | `heartbeat_hardware_has_cores` | — | — | — | — | — | — | — | |
| 324 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 175 | `first_create_and_second_load_return_same_uuid` | — | — | — | — | — | — | — | |
| 325 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 185 | `deleting_machine_token_does_not_change_machine_id` | — | — | — | — | — | — | — | |
| 326 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 197 | `stored_id_with_surrounding_whitespace_is_reused` | — | — | — | — | — | — | — | |
| 327 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 207 | `created_machine_id_file_is_mode_0600` | — | — | — | — | — | — | — | |
| 328 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 217 | `machine_id_stays_on_config_path_after_install_dir_swap` | — | — | — | — | — | — | — | |
| 329 | `unit` | `default` | `crates/fleet-host/src/paths.rs` | 252 | `acquire_run_lock_is_exclusive_for_same_host_id` | — | — | — | — | — | — | — | |
| 330 | `unit` | `default` | `crates/fleet-host/src/platform.rs` | 241 | `platform_detect_does_not_panic` | — | — | — | — | — | — | — | |
| 331 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 659 | `persist_pair_keeps_non_loopback_url_when_response_is_localhost` | — | — | — | — | — | — | — | |
| 332 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 679 | `persist_pair_accepts_non_loopback_response` | — | — | — | — | — | — | — | |
| 333 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 699 | `acquire_run_lock_refuses_second_process` | — | — | — | — | — | — | — | |
| 334 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 713 | `is_loopback_url_detects_common_forms` | — | — | — | — | — | — | — | |
| 335 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 731 | `persist_pair_rejects_empty_machine_token` | — | — | — | — | — | — | — | |
| 336 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 754 | `persist_pair_never_stores_unusable_bind_all_url` | — | — | — | — | — | — | — | |
| 337 | `unit` | `default` | `crates/fleet-host/src/run.rs` | 782 | `persist_pair_treats_ipv6_loopback_response_as_loopback` | — | — | — | — | — | — | — | |
| 338 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 153 | `pkg_version_from_bytes_reads_stamp` | — | — | — | — | — | — | — | |
| 339 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 174 | `driver_version_prefers_on_disk_stamp` | — | — | — | — | — | — | — | |
| 340 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 187 | `heartbeat_software_always_sends_git_sha_key` | — | — | — | — | — | — | — | |
| 341 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 208 | `heartbeat_driver_version_only_from_on_disk_stamp` | — | — | — | — | — | — | — | |
| 342 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 246 | `file_sha256_hashes_bytes` | — | — | — | — | — | — | — | |
| 343 | `unit` | `default` | `crates/fleet-host/src/software.rs` | 259 | `heartbeat_software_build_id_is_never_file_hash` | — | — | — | — | — | — | — | |
| 344 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 652 | `extra_env_cannot_replace_fleet_worker_token` | — | — | — | — | — | — | — | |
| 345 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 669 | `child_env_defaults_harness_to_mock` | — | — | — | — | — | — | — | |
| 346 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 711 | `child_env_overwrites_payload_fleet_work_dir` | — | — | — | — | — | — | — | |
| 347 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 721 | `vm_not_allowed_without_hypervisor` | — | — | — | — | — | — | — | |
| 348 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 744 | `container_runtime_rejected_when_run_container_false` | — | — | — | — | — | — | — | |
| 349 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 770 | `isolation_string_selects_runtime` | — | — | — | — | — | — | — | |
| 350 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 787 | `docker_missing_errors_without_host_fallback` | — | — | — | — | — | — | — | |
| 351 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 803 | `in_cluster_picks_kube_pod` | — | — | — | — | — | — | — | |
| 352 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 818 | `docker_args_use_bridge_network_and_fleet_env` | — | — | — | — | — | — | — | |
| 353 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 847 | `isolated_guest_drops_host_absolute_bin_overrides` | — | — | — | — | — | — | — | |
| 354 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 897 | `collect_guest_proof_prefers_result_md` | — | — | — | — | — | — | — | |
| 355 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 909 | `collect_guest_proof_synthesizes_from_artifacts` | — | — | — | — | — | — | — | |
| 356 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 923 | `collect_guest_proof_none_without_artifacts` | — | — | — | — | — | — | — | |
| 357 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 929 | `docker_network_none_is_refused_because_guests_must_reach_the_plane` | — | — | — | — | — | — | — | |
| 358 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 940 | `container_claim_refuses_loopback_orchestrator_url` | — | — | — | — | — | — | — | |
| 359 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 960 | `run_claim_refuses_privileged_when_not_allowed` | — | — | — | — | — | — | — | |
| 360 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 980 | `docker_args_privileged_only_when_allowed` | — | — | — | — | — | — | — | |
| 361 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 992 | `run_claim_container_docker_missing` | — | — | — | — | — | — | — | |
| 362 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1009 | `run_claim_container_runtime_rejected_when_run_container_false` | — | — | — | — | — | — | — | |
| 363 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1047 | `run_claim_vm_without_hypervisor_bails` | — | — | — | — | — | — | — | |
| 364 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1065 | `run_vm_fails_closed` | — | — | — | — | — | — | — | |
| 365 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1077 | `kill_session_missing_is_ok` | — | — | — | — | — | — | — | |
| 366 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1083 | `kill_session_sigterms_host_child` | — | — | — | — | — | — | — | |
| 367 | `unit` | `default` | `crates/fleet-host/src/spawn.rs` | 1102 | `tracks_docker_session_then_forget` | — | — | — | — | — | — | — | |
| 368 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 883 | `hash_match_is_noop` | — | — | — | — | — | — | — | |
| 369 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 902 | `checksum_fail_leaves_old_bins` | — | — | — | — | — | — | — | |
| 370 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 928 | `child_only_apply_does_not_require_jobs_idle` | — | — | — | — | — | — | — | |
| 371 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 955 | `local_auto_update_false_does_not_apply` | — | — | — | — | — | — | — | |
| 372 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1048 | `update_now_is_one_shot_when_auto_off` | — | — | — | — | — | — | — | |
| 373 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1111 | `windows_updater_script_written_running_exe_not_opened` | — | — | — | — | — | — | — | |
| 374 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1151 | `swap_staged_renames_open_dest_aside` | — | — | — | — | — | — | — | |
| 375 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1165 | `pe_machine_type_amd64_and_arm64` | — | — | — | — | — | — | — | |
| 376 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1185 | `one_attempt_per_desired_sha` | — | — | — | — | — | — | — | |
| 377 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1256 | `apply_does_not_patch_drain_or_paused` | — | — | — | — | — | — | — | |
| 378 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1304 | `host_apply_sets_updating_and_does_not_http_patch` | — | — | — | — | — | — | — | |
| 379 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1351 | `maybe_queue_auto_respects_local_flag` | — | — | — | — | — | — | — | |
| 380 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1461 | `update_holds_ready_auto_and_now_do_not_rotate_machine_token` | — | — | — | — | — | — | — | |
| 381 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1591 | `update_holds_ready_mid_swap_does_not_mint_machine_token` | — | — | — | — | — | — | — | |
| 382 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1664 | `update_now_mid_swap_does_not_keep_pulling_when_auto_off` | — | — | — | — | — | — | — | |
| 383 | `unit` | `default` | `crates/fleet-host/src/update.rs` | 1788 | `update_now_failed_mid_swap_does_not_keep_pulling_when_auto_off` | — | — | — | — | — | — | — | |

## fleet-integrations

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 384 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 302 | `catalog_has_unique_ids` | — | — | — | — | — | — | — | |
| 385 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 317 | `oauth_only` | — | — | — | — | — | — | — | |
| 386 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 324 | `gitlab_is_available` | — | — | — | — | — | — | — | |
| 387 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 334 | `trello_is_available` | — | — | — | — | — | — | — | |
| 388 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 344 | `asana_is_available` | — | — | — | — | — | — | — | |
| 389 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 354 | `no_em_dash_in_copy` | — | — | — | — | — | — | — | |
| 390 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 369 | `confluence_is_coming_soon_and_must_not_expose_oauth_start` | — | — | — | — | — | — | — | |
| 391 | `unit` | `default` | `crates/fleet-integrations/src/catalog.rs` | 448 | `bitbucket_is_coming_soon_and_must_not_expose_oauth_start` | — | — | — | — | — | — | — | |
| 392 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 242 | `jwt_secret_unset_and_whitespace_fails_from_env` | — | — | — | — | — | — | — | |
| 393 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 260 | `jwt_secret_shorter_than_32_fails_from_env` | — | — | — | — | — | — | — | |
| 394 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 272 | `valid_secret_from_env_works` | — | — | — | — | — | — | — | |
| 395 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 282 | `jwt_secret_is_trimmed_before_length_check` | — | — | — | — | — | — | — | |
| 396 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 416 | `integrations_encryption_key_short_fails_from_env` | — | — | — | — | — | — | — | |
| 397 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 431 | `integrations_encryption_key_from_env_valid_and_unset` | — | — | — | — | — | — | — | |
| 398 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 589 | `oauth_client_pair_half_set_fails_from_env` | — | — | — | — | — | — | — | |
| 399 | `unit` | `default` | `crates/fleet-integrations/src/config.rs` | 639 | `oauth_client_pair_both_unset_or_set_from_env` | — | — | — | — | — | — | — | |
| 400 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 167 | `roundtrip` | — | — | — | — | — | — | — | |
| 401 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 182 | `legacy_plaintext_passthrough` | — | — | — | — | — | — | — | |
| 402 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 190 | `hex_key_accepted` | — | — | — | — | — | — | — | |
| 403 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 199 | `wrong_key_fails` | — | — | — | — | — | — | — | |
| 404 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 212 | `truncated_ciphertext_fails` | — | — | — | — | — | — | — | |
| 405 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 228 | `garbled_ciphertext_fails` | — | — | — | — | — | — | — | |
| 406 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 248 | `empty_ciphertext_fails` | — | — | — | — | — | — | — | |
| 407 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 259 | `integrations_encryption_key_from_secret_rejects_short` | — | — | — | — | — | — | — | |
| 408 | `unit` | `default` | `crates/fleet-integrations/src/crypto.rs` | 274 | `integrations_encryption_key_from_secret_passphrase_and_hex` | — | — | — | — | — | — | — | |
| 409 | `unit` | `default` | `crates/fleet-integrations/src/oauth/callback_error.rs` | 137 | `status_is_derived_from_variant_not_message_text` | — | — | — | — | — | — | — | |
| 410 | `unit` | `default` | `crates/fleet-integrations/src/oauth/callback_error.rs` | 170 | `app_error_maps_by_variant` | — | — | — | — | — | — | — | |
| 411 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 261 | `app_name_fits_github_limit` | — | — | — | — | — | — | — | |
| 412 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 269 | `manifest_points_at_local_callbacks` | — | — | — | — | — | — | — | |
| 413 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 289 | `manifest_omits_private_webhook` | — | — | — | — | — | — | — | |
| 414 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 317 | `form_html_includes_manifest_and_github_action` | — | — | — | — | — | — | — | |
| 415 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 325 | `install_url_encodes_state` | — | — | — | — | — | — | — | |
| 416 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 366 | `github_app_install_url_is_not_user_oauth_authorize_url` | — | — | — | — | — | — | — | |
| 417 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 395 | `manifest_form_action_is_not_user_oauth_authorize_url` | — | — | — | — | — | — | — | |
| 418 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 420 | `authorize_with_app_slug_uses_install_url_not_user_oauth` | — | — | — | — | — | — | — | |
| 419 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 437 | `manifest_callback_is_not_user_oauth_callback` | — | — | — | — | — | — | — | |
| 420 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 476 | `manifest_state_cannot_complete_user_oauth_callback` | — | — | — | — | — | — | — | |
| 421 | `unit` | `default` | `crates/fleet-integrations/src/oauth/github_app.rs` | 492 | `user_oauth_state_cannot_complete_manifest_callback` | — | — | — | — | — | — | — | |
| 422 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 83 | `issue_and_verify_round_trip` | — | — | — | — | — | — | — | |
| 423 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 99 | `wrong_secret_rejected` | — | — | — | — | — | — | — | |
| 424 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 108 | `tampered_signature_rejected` | — | — | — | — | — | — | — | |
| 425 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 119 | `tampered_payload_rejected_even_if_provider_changed` | — | — | — | — | — | — | — | |
| 426 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 133 | `access_token_typ_cannot_be_used_as_oauth_state` | — | — | — | — | — | — | — | |
| 427 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 178 | `expired_oauth_state_is_unauthorized` | — | — | — | — | — | — | — | |
| 428 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 211 | `oauth_state_nonce_is_unique_per_issued_token` | — | — | — | — | — | — | — | |
| 429 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 240 | `oauth_state_verify_does_not_consume_nonce` | — | — | — | — | — | — | — | |
| 430 | `unit` | `default` | `crates/fleet-integrations/src/oauth/state_token.rs` | 255 | `oauth_state_empty_nonce_is_rejected` | — | — | — | — | — | — | — | |
| 431 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/bitbucket/client.rs` | 164 | `repo_from_owner_repo_or_full_name` | — | — | — | — | — | — | — | |
| 432 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/gitlab/client.rs` | 66 | `project_from_path_owner_repo_or_id` | — | — | — | — | — | — | — | |
| 433 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 118 | `supported_providers_has_unique_ids` | — | — | — | — | — | — | — | |
| 434 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 131 | `bitbucket_and_confluence_ops_registered` | — | — | — | — | — | — | — | |
| 435 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 137 | `github_operations_include_commit_files_and_create_pr` | — | — | — | — | — | — | — | |
| 436 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 144 | `slack_and_notion_have_test_trio_ops` | — | — | — | — | — | — | — | |
| 437 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 157 | `unknown_provider_has_no_operations` | — | — | — | — | — | — | — | |
| 438 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/mod.rs` | 162 | `operations_catalog_lists_every_provider` | — | — | — | — | — | — | — | |
| 439 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/notion/client.rs` | 140 | `block_plain_text_reads_paragraph` | — | — | — | — | — | — | — | |
| 440 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/notion/client.rs` | 154 | `write_text_accepts_children_strings_and_text` | — | — | — | — | — | — | — | |
| 441 | `unit` | `default` | `crates/fleet-integrations/src/proxy/apps/slack/tools/conversations.rs` | 359 | `matches_hash_eng_name` | — | — | — | — | — | — | — | |
| 442 | `unit` | `default` | `crates/fleet-integrations/src/proxy/mod.rs` | 349 | `operations_listed` | — | — | — | — | — | — | — | |
| 443 | `unit` | `default` | `crates/fleet-integrations/src/proxy/mod.rs` | 381 | `context_markdown_no_secrets` | — | — | — | — | — | — | — | |
| 444 | `unit` | `default` | `crates/fleet-integrations/src/proxy/mod.rs` | 401 | `github_create_pr_allowlisted_unknown_decrypt_sanitized` | — | — | — | — | — | — | — | |
| 445 | `unit` | `default` | `crates/fleet-integrations/src/proxy/mod.rs` | 731 | `integration_query_decrypts_server_side_and_sanitizes_tokens` | — | — | — | — | — | — | — | |
| 446 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 123 | `keeps_pagination_tokens` | — | — | — | — | — | — | — | |
| 447 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 136 | `strips_tokens` | — | — | — | — | — | — | — | |
| 448 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 149 | `strips_secret_keys_from_sample_json` | — | — | — | — | — | — | — | |
| 449 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 186 | `strips_bare_oidc_id_token` | — | — | — | — | — | — | — | |
| 450 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 206 | `strips_camelcase_oidc_id_token` | — | — | — | — | — | — | — | |
| 451 | `unit` | `default` | `crates/fleet-integrations/src/proxy/sanitize.rs` | 228 | `caps_large_item_lists` | — | — | — | — | — | — | — | |
| 452 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1064 | `callback_status_is_typed_and_user_mismatch_is_named` | — | — | — | — | — | — | — | |
| 453 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1131 | `patch_agent_defaults_only_updates_connected_never_inserts` | — | — | — | — | — | — | — | |
| 454 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1286 | `disconnect_unknown_provider_is_rejected_before_repo` | — | — | — | — | — | — | — | |
| 455 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1294 | `disconnect_response_does_not_serialize_credentials` | — | — | — | — | — | — | — | |
| 456 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1310 | `disconnect_cleared_credentials_fail_closed_for_proxy` | — | — | — | — | — | — | — | |
| 457 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1348 | `confluence_get_oauth_start_is_client_error` | — | — | — | — | — | — | — | |
| 458 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1430 | `oauth_start_unconfigured_still_bootstraps` | — | — | — | — | — | — | — | |
| 459 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1681 | `token_paste_not_accepted_except_trello_oauth_user_token_flow` | — | — | — | — | — | — | — | |
| 460 | `unit` | `default` | `crates/fleet-integrations/src/routes.rs` | 1827 | `oauth_app_put_and_start_form_never_return_client_secret` | — | — | — | — | — | — | — | |
| 461 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 375 | `oauth_callback_state_workspace_must_match_session_workspace` | — | — | — | — | — | — | — | |
| 462 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 488 | `oauth_callback_requires_session_cookie` | — | — | — | — | — | — | — | |
| 463 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 624 | `oauth_expired_state_must_not_complete_connect` | — | — | — | — | — | — | — | |
| 464 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 763 | `oauth_callback_credentials_encrypted_at_rest` | — | — | — | — | — | — | — | |
| 465 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 946 | `oauth_callback_provider_mismatch_is_400` | — | — | — | — | — | — | — | |
| 466 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 987 | `oauth_callback_nonce_reuse_is_409` | — | — | — | — | — | — | — | |
| 467 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_callback_workspace_bind.rs` | 1075 | `github_manifest_callback_requires_session_user_match` | — | — | — | — | — | — | — | |
| 468 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_state_reuse.rs` | 237 | `oauth_state_nonce_must_not_complete_connect_twice` | — | — | — | — | — | — | — | |
| 469 | `rust-integration` | `default` | `crates/fleet-integrations/tests/oauth_state_reuse.rs` | 344 | `oauth_state_parallel_callback_consumes_nonce_once` | — | — | — | — | — | — | — | |
| 470 | `rust-integration` | `default` | `crates/fleet-integrations/tests/proxy_workspace_isolation.rs` | 81 | `invoke_workspace_b_cannot_use_workspace_a_connection` | — | — | — | — | — | — | — | |
| 471 | `rust-integration` | `default` | `crates/fleet-integrations/tests/proxy_workspace_isolation.rs` | 174 | `integration_query_decrypts_server_side_and_never_echoes_token` | — | — | — | — | — | — | — | |

## fleet-operator

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 472 | `unit` | `default` | `crates/fleet-operator/src/crd.rs` | 154 | `dns_label_sanitizes` | — | — | — | — | — | — | — | |
| 473 | `unit` | `default` | `crates/fleet-operator/src/crd.rs` | 161 | `crd_identity` | — | — | — | — | — | — | — | |
| 474 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 562 | `pod_has_worker_contract_and_security` | — | — | — | — | — | — | — | |
| 475 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 587 | `network_policy_denies_ingress_and_limits_egress` | — | — | — | — | — | — | — | |
| 476 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 779 | `network_policy_allowlist_cidrs_not_hostnames` | — | — | — | — | — | — | — | |
| 477 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 897 | `ttl_seconds_exceeded_deletes_the_cr` | — | — | — | — | — | — | — | |
| 478 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 987 | `ttl_expired_deletes_sandbox_session` | — | — | — | — | — | — | — | |
| 479 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 999 | `ttl_missing_does_not_delete_early` | — | — | — | — | — | — | — | |
| 480 | `unit` | `default` | `crates/fleet-operator/src/reconcile.rs` | 1022 | `ttl_finalizer_cleanup_targets_pod_and_network_policy` | — | — | — | — | — | — | — | |

## fleet-orchestrator

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 481 | `unit` | `default` | `crates/fleet-orchestrator/src/api/handlers.rs` | 724 | `healthz_ok_without_db_readyz_503_when_pool_cannot_acquire` | — | — | — | — | — | — | — | |
| 482 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 273 | `load_defaults` | — | — | — | — | — | — | — | |
| 483 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 294 | `requires_database_url` | — | — | — | — | — | — | — | |
| 484 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 304 | `admin_token_optional_in_token_mode` | — | — | — | — | — | — | — | |
| 485 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 317 | `auth_none_requires_loopback` | — | — | — | — | — | — | — | |
| 486 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 332 | `public_url_rewrites_wildcard_and_prefers_env` | — | — | — | — | — | — | — | |
| 487 | `unit` | `default` | `crates/fleet-orchestrator/src/config/mod.rs` | 360 | `from_env_snapshots_public_url_onto_config` | — | — | — | — | — | — | — | |
| 488 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/capacity.rs` | 37 | `free_slots_cases` | — | — | — | — | — | — | — | |
| 489 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/capacity.rs` | 46 | `count_in_use` | — | — | — | — | — | — | — | |
| 490 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/capacity.rs` | 63 | `sessions_in_spawning_healthy_stuck_killing_hold_capacity_slots` | — | — | — | — | — | — | — | |
| 491 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/deps.rs` | 107 | `fan_in_deps_cap_32_cycles_rejected` | — | — | — | — | — | — | — | |
| 492 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/deps.rs` | 214 | `evaluate_matrix` | — | — | — | — | — | — | — | |
| 493 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 159 | `empty_url_clears_binding` | — | — | — | — | — | — | — | |
| 494 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 171 | `https_ok` | — | — | — | — | — | — | — | |
| 495 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 180 | `default_ref_is_main` | — | — | — | — | — | — | — | |
| 496 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 188 | `rejects_garbage_url` | — | — | — | — | — | — | — | |
| 497 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 211 | `rejects_http_userinfo_without_echoing_secret` | — | — | — | — | — | — | — | |
| 498 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 244 | `rejects_invalid_git_ref` | — | — | — | — | — | — | — | |
| 499 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 253 | `apply_git_remote_patch_empty_url_clears_git_ref_without_url_and_garbage_url_are_errors` | — | — | — | — | — | — | — | |
| 500 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/git_remote.rs` | 326 | `cookie_and_admin_patch_share_apply_git_remote_patch` | — | — | — | — | — | — | — | |
| 501 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/ids.rs` | 20 | `deterministic` | — | — | — | — | — | — | — | |
| 502 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/ids.rs` | 30 | `different_inputs_differ` | — | — | — | — | — | — | — | |
| 503 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/ids.rs` | 45 | `golden` | — | — | — | — | — | — | — | |
| 504 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/isolation.rs` | 10 | `k8s_runtime_maps_to_container_not_its_own_class` | — | — | — | — | — | — | — | |
| 505 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/isolation.rs` | 19 | `workspace_vm_need_rejects_process_only_host` | — | — | — | — | — | — | — | |
| 506 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/retry.rs` | 98 | `total_attempts` | — | — | — | — | — | — | — | |
| 507 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/retry.rs` | 106 | `retry_matrix` | — | — | — | — | — | — | — | |
| 508 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 234 | `can_transition_matrix` | — | — | — | — | — | — | — | |
| 509 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 296 | `self_transition_not_allowed` | — | — | — | — | — | — | — | |
| 510 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 303 | `is_terminal` | — | — | — | — | — | — | — | |
| 511 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 312 | `transition_errors` | — | — | — | — | — | — | — | |
| 512 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 318 | `illegal_task_transition_rejected` | — | — | — | — | — | — | — | |
| 513 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 413 | `session_capacity` | — | — | — | — | — | — | — | |
| 514 | `unit` | `default` | `crates/fleet-orchestrator/src/domain/status.rs` | 423 | `plan_accepted` | — | — | — | — | — | — | — | |
| 515 | `unit` | `default` | `crates/fleet-orchestrator/src/notify/mod.rs` | 71 | `noop_ok` | — | — | — | — | — | — | — | |
| 516 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/board_sync_step.rs` | 126 | `skips_when_disabled` | — | — | — | — | — | — | — | |
| 517 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/board_sync_step.rs` | 138 | `syncs_tasks_with_external_ref` | — | — | — | — | — | — | — | |
| 518 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/deps_step.rs` | 147 | `thirty_three_parent_join_or_cycle_must_not_enter_store` | — | — | — | — | — | — | — | |
| 519 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/deps_step.rs` | 366 | `promotes_root_and_waits_on_child` | — | — | — | — | — | — | — | |
| 520 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/deps_step.rs` | 389 | `fail_forward_when_dep_failed` | — | — | — | — | — | — | — | |
| 521 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/inbox.rs` | 334 | `materialize_writes_result_md_without_inventing_work` | — | — | — | — | — | — | — | |
| 522 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/inbox.rs` | 353 | `materialize_rejects_parent_paths` | — | — | — | — | — | — | — | |
| 523 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/inbox.rs` | 370 | `materialize_without_proof_does_not_invent_result` | — | — | — | — | — | — | — | |
| 524 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/materialize_step.rs` | 60 | `eligibility_matrix` | — | — | — | — | — | — | — | |
| 525 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/materialize_step.rs` | 82 | `only_accepted_plans_materialize` | — | — | — | — | — | — | — | |
| 526 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/mod.rs` | 146 | `apply_task_terminal_store_and_callers_require_assigned_in_progress_claim` | — | — | — | — | — | — | — | |
| 527 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/review.rs` | 119 | `changes_request_deserializes` | — | — | — | — | — | — | — | |
| 528 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 91 | `keep_active_while_running` | — | — | — | — | — | — | — | |
| 529 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 101 | `blocked_keeps_active` | — | — | — | — | — | — | — | |
| 530 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 111 | `all_success_completed` | — | — | — | — | — | — | — | |
| 531 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 121 | `fail_run_on_done_failed` | — | — | — | — | — | — | — | |
| 532 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 131 | `leave_policy_completes_with_failures` | — | — | — | — | — | — | — | |
| 533 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 141 | `on_task_fail_leave_does_not_fail_run` | — | — | — | — | — | — | — | |
| 534 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 219 | `k27_cancel_beats_all_success` | — | — | — | — | — | — | — | |
| 535 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 229 | `cancel_before_materialize_empty_tasks` | — | — | — | — | — | — | — | |
| 536 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/rollup.rs` | 240 | `cancelled_mix_without_operator_cancel_is_failed` | — | — | — | — | — | — | — | |
| 537 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 293 | `acceptance_array_of_strings` | — | — | — | — | — | — | — | |
| 538 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 302 | `acceptance_non_array_empty` | — | — | — | — | — | — | — | |
| 539 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 308 | `compose_includes_sections` | — | — | — | — | — | — | — | |
| 540 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 322 | `pass_through_does_not_copy_fleet_harness` | — | — | — | — | — | — | — | |
| 541 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 335 | `write_files_roundtrip` | — | — | — | — | — | — | — | |
| 542 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 356 | `write_files_includes_repo` | — | — | — | — | — | — | — | |
| 543 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/session_context.rs` | 379 | `run_context_names_every_task` | — | — | — | — | — | — | — | |
| 544 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/spawn.rs` | 302 | `loopback_detects_ipv4_ipv6_and_localhost` | — | — | — | — | — | — | — | |
| 545 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 289 | `stub_always_passes` | — | — | — | — | — | — | — | |
| 546 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 296 | `k8s_and_remote_still_require_proof_of_work` | — | — | — | — | — | — | — | |
| 547 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 320 | `process_requires_result_md` | — | — | — | — | — | — | — | |
| 548 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 328 | `process_passes_with_result_and_work` | — | — | — | — | — | — | — | |
| 549 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 342 | `exists_rejects_parent_escape` | — | — | — | — | — | — | — | |
| 550 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 347 | `exists_and_contains_checks` | — | — | — | — | — | — | — | |
| 551 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 358 | `missing_contains_fails` | — | — | — | — | — | — | — | |
| 552 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 369 | `ship_id_does_not_require_a_github_url` | — | — | — | — | — | — | — | |
| 553 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 384 | `generalist_default_acceptance_is_result_only` | — | — | — | — | — | — | — | |
| 554 | `unit` | `default` | `crates/fleet-orchestrator/src/orch/validate_done.rs` | 392 | `apply_task_terminal_only_from_assigned_in_progress_with_matching_claimed_session_id` | — | — | — | — | — | — | — | |
| 555 | `unit` | `default` | `crates/fleet-orchestrator/src/personas.rs` | 393 | `frontmatter_parse` | — | — | — | — | — | — | — | |
| 556 | `unit` | `default` | `crates/fleet-orchestrator/src/personas.rs` | 402 | `pick_aliases` | — | — | — | — | — | — | — | |
| 557 | `unit` | `default` | `crates/fleet-orchestrator/src/personas.rs` | 418 | `title_case_slug` | — | — | — | — | — | — | — | |
| 558 | `unit` | `default` | `crates/fleet-orchestrator/src/personas.rs` | 427 | `scan_dir_reads_frontmatter_and_skips_readme` | — | — | — | — | — | — | — | |
| 559 | `unit` | `default` | `crates/fleet-orchestrator/src/personas.rs` | 449 | `resolve_uses_personas_dir_without_network` | — | — | — | — | — | — | — | |
| 560 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1479 | `settings_off_is_bash_only_in_tests` | — | — | — | — | — | — | — | |
| 561 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1485 | `settings_ollama_picks_model` | — | — | — | — | — | — | — | |
| 562 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1492 | `chat_selection_wins_over_settings` | — | — | — | — | — | — | — | |
| 563 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1504 | `future_claude_provider_is_preserved` | — | — | — | — | — | — | — | |
| 564 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1511 | `plan_grok_wins_over_workspace_ollama_and_off` | — | — | — | — | — | — | — | |
| 565 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1524 | `normalize_provider_maps_aliases_and_off` | — | — | — | — | — | — | — | |
| 566 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1535 | `grok_env_is_forced_off_in_unit_tests` | — | — | — | — | — | — | — | |
| 567 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1543 | `safe_path_rejects_parent_escape` | — | — | — | — | — | — | — | |
| 568 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1556 | `safe_path_rejects_symlink_dir_escape` | — | — | — | — | — | — | — | |
| 569 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1565 | `run_bash_times_out_and_kills` | — | — | — | — | — | — | — | |
| 570 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1572 | `append_capped_does_not_reread_on_small_write` | — | — | — | — | — | — | — | |
| 571 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1582 | `truncate_appends_marker` | — | — | — | — | — | — | — | |
| 572 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1599 | `exec_tool_read_write_list_and_blocks_escape` | — | — | — | — | — | — | — | |
| 573 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1634 | `exec_tool_send_and_read_across_teammates` | — | — | — | — | — | — | — | |
| 574 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1660 | `exec_tool_request_changes_writes_review` | — | — | — | — | — | — | — | |
| 575 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1695 | `exec_tool_use_integration_goes_through_control_plane` | — | — | — | — | — | — | — | |
| 576 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1717 | `ship_helpers_find_pr_owner_and_skip_junk` | — | — | — | — | — | — | — | |
| 577 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1760 | `run_ship_reuses_existing_pr_without_io` | — | — | — | — | — | — | — | |
| 578 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/agent_loop.rs` | 1785 | `agent_job_abort_sets_exit_143` | — | — | — | — | — | — | — | |
| 579 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/git_isolate.rs` | 287 | `already_spawned_keep_workdir_on_remote_change` | — | — | — | — | — | — | — | |
| 580 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/git_isolate.rs` | 408 | `new_spawn_skips_clone_when_remote_cleared` | — | — | — | — | — | — | — | |
| 581 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/git_isolate.rs` | 498 | `worktree_from_local_repo` | — | — | — | — | — | — | — | |
| 582 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/io.rs` | 244 | `slack_thread_reply` | — | — | — | — | — | — | — | |
| 583 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/io.rs` | 260 | `notion_append` | — | — | — | — | — | — | — | |
| 584 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/io.rs` | 275 | `github_comment` | — | — | — | — | — | — | — | |
| 585 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/io.rs` | 290 | `subst_templates` | — | — | — | — | — | — | — | |
| 586 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/mod.rs` | 106 | `fleet_env_keys` | — | — | — | — | — | — | — | |
| 587 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/remote.rs` | 160 | `handle_roundtrip` | — | — | — | — | — | — | — | |
| 588 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/remote.rs` | 171 | `payload_includes_plaintext_token_and_env` | — | — | — | — | — | — | — | |
| 589 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/share.rs` | 247 | `copy_skips_control_files_and_keeps_code` | — | — | — | — | — | — | — | |
| 590 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/share.rs` | 265 | `new_spawn_skips_clone_when_remote_cleared` | — | — | — | — | — | — | — | |
| 591 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/share.rs` | 396 | `seed_empty_workspace_and_roundtrip_repo` | — | — | — | — | — | — | — | |
| 592 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/share.rs` | 425 | `changes_request_roundtrip` | — | — | — | — | — | — | — | |
| 593 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team.rs` | 374 | `team_md_marks_you_and_lists_others` | — | — | — | — | — | — | — | |
| 594 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team.rs` | 414 | `excerpt_truncates_long_handoffs` | — | — | — | — | — | — | — | |
| 595 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team.rs` | 423 | `archive_and_copy_dep_handoffs` | — | — | — | — | — | — | — | |
| 596 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team.rs` | 494 | `prepare_workspace_writes_team_md` | — | — | — | — | — | — | — | |
| 597 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 421 | `send_is_unread_for_target_not_sender` | — | — | — | — | — | — | — | |
| 598 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 438 | `broadcast_reaches_everyone_else` | — | — | — | — | — | — | — | |
| 599 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 454 | `ask_and_reply_roundtrip` | — | — | — | — | — | — | — | |
| 600 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 477 | `normalize_to_aliases` | — | — | — | — | — | — | — | |
| 601 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 484 | `run_dir_is_project_and_run_scoped` | — | — | — | — | — | — | — | |
| 602 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 504 | `distinct_projects_do_not_share_chat_jsonl` | — | — | — | — | — | — | — | |
| 603 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/team_chat.rs` | 521 | `format_inbox_includes_ask` | — | — | — | — | — | — | — | |
| 604 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/worker_script.rs` | 208 | `extracts_bash_fences` | — | — | — | — | — | — | — | |
| 605 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/worker_script.rs` | 230 | `worker_runs_commands_from_work_root` | — | — | — | — | — | — | — | |
| 606 | `unit` | `default` | `crates/fleet-orchestrator/src/sandbox/worker_script.rs` | 237 | `spec_md_from_env_writes_when_absent` | — | — | — | — | — | — | — | |
| 607 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 637 | `this_machine_matches_hostname_fqdn_and_addrs` | — | — | — | — | — | — | — | |
| 608 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 663 | `loopback_reachable_keeps_this_machine_drops_offbox_host` | — | — | — | — | — | — | — | |
| 609 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 693 | `v1_assigns_host_even_when_container_is_advertised` | — | — | — | — | — | — | — | |
| 610 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 699 | `container_only_host_is_assignable` | — | — | — | — | — | — | — | |
| 611 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 713 | `container_need_does_not_fall_back_to_process` | — | — | — | — | — | — | — | |
| 612 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 724 | `container_need_picks_container_when_both_offered` | — | — | — | — | — | — | — | |
| 613 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 732 | `host_when_container_not_advertised` | — | — | — | — | — | — | — | |
| 614 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 738 | `skip_when_plane_disallows_both` | — | — | — | — | — | — | — | |
| 615 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 744 | `none_when_host_allowed_but_not_advertised_and_no_container` | — | — | — | — | — | — | — | |
| 616 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 750 | `skip_at_max_concurrent_and_256_cap` | — | — | — | — | — | — | — | |
| 617 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 760 | `skip_when_advertised_cap_reached` | — | — | — | — | — | — | — | |
| 618 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 772 | `pick_skips_full_and_ineffective` | — | — | — | — | — | — | — | |
| 619 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 789 | `pick_lowest_utilization_not_first_under_capacity` | — | — | — | — | — | — | — | |
| 620 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 803 | `pick_equal_load_prefers_oldest_not_newest_first` | — | — | — | — | — | — | — | |
| 621 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 816 | `vm_need_is_unassigned_when_no_host_offers_vm` | — | — | — | — | — | — | — | |
| 622 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 825 | `pick_none_when_all_full` | — | — | — | — | — | — | — | |
| 623 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 834 | `timeout_is_min_task_and_host_cap` | — | — | — | — | — | — | — | |
| 624 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 841 | `hold_ready_when_home_host_is_updating` | — | — | — | — | — | — | — | |
| 625 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 852 | `runner_default_is_native_and_picks_paired_host` | — | — | — | — | — | — | — | |
| 626 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 865 | `runner_skips_sandboxed_and_picks_native` | — | — | — | — | — | — | — | |
| 627 | `unit` | `default` | `crates/fleet-orchestrator/src/store/host_pool.rs` | 924 | `from_worker_host_maps_isolation` | — | — | — | — | — | — | — | |
| 628 | `unit` | `default` | `crates/fleet-orchestrator/src/store/mod.rs` | 781 | `token_hash_roundtrip` | — | — | — | — | — | — | — | |
| 629 | `unit` | `default` | `crates/fleet-orchestrator/src/store/tick_ops.rs` | 1110 | `set_max_agents_is_greatest_raise_only` | — | — | — | — | — | — | — | |
| 630 | `unit` | `default` | `crates/fleet-orchestrator/src/sync/io.rs` | 451 | `parses_github_and_gitlab_keys` | — | — | — | — | — | — | — | |
| 631 | `unit` | `default` | `crates/fleet-orchestrator/src/sync/io.rs` | 464 | `picks_jira_and_linear_targets` | — | — | — | — | — | — | — | |
| 632 | `unit` | `default` | `crates/fleet-orchestrator/src/sync/io.rs` | 537 | `moves_jira_linear_github_cards` | — | — | — | — | — | — | — | |
| 633 | `unit` | `default` | `crates/fleet-orchestrator/src/sync/mod.rs` | 61 | `noop_sync` | — | — | — | — | — | — | — | |
| 634 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 194 | `ok` | — | — | — | — | — | — | — | |
| 635 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 199 | `missing_dependency` | — | — | — | — | — | — | — | |
| 636 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 207 | `cycle` | — | — | — | — | — | — | — | |
| 637 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 242 | `self_cycle` | — | — | — | — | — | — | — | |
| 638 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 261 | `duplicate_id` | — | — | — | — | — | — | — | |
| 639 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 280 | `empty_tasks` | — | — | — | — | — | — | — | |
| 640 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 287 | `max_tasks` | — | — | — | — | — | — | — | |
| 641 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 308 | `max_spec_md` | — | — | — | — | — | — | — | |
| 642 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 315 | `max_fan_in` | — | — | — | — | — | — | — | |
| 643 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 354 | `missing_title` | — | — | — | — | — | — | — | |
| 644 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 361 | `json_roundtrip` | — | — | — | — | — | — | — | |
| 645 | `unit` | `default` | `crates/fleet-orchestrator/src/validate/mod.rs` | 369 | `body_size` | — | — | — | — | — | — | — | |
| 646 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 203 | `healthz_ok` | — | — | — | — | — | — | — | |
| 647 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 211 | `fixture_and_tick_happy_path` | — | — | — | — | — | — | — | |
| 648 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 357 | `admin_auth_required` | — | — | — | — | — | — | — | |
| 649 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 375 | `worker_protocol_rejects_missing_empty_wrong_key_and_truncated_token` | — | — | — | — | — | — | — | |
| 650 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 598 | `worker_heartbeat_status_wrong_token_401_other_project_404` | — | — | — | — | — | — | — | |
| 651 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/api_integration.rs` | 997 | `machine_token_v1_worker_heartbeat_status_rejected` | — | — | — | — | — | — | — | |
| 652 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 44 | `default_isolation_is_native` | — | — | — | — | — | — | — | |
| 653 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 52 | `wrapper_picks_paired_native_host` | — | — | — | — | — | — | — | |
| 654 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 65 | `sandboxed_is_not_picked` | — | — | — | — | — | — | — | |
| 655 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 74 | `skips_sandboxed_and_picks_following_native` | — | — | — | — | — | — | — | |
| 656 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 85 | `only_self_hosted_native_kind_is_assignable` | — | — | — | — | — | — | — | |
| 657 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/assignable_runner.rs` | 107 | `select_assignable_runner_ignores_planted_cloud_partners_public_cloud` | — | — | — | — | — | — | — | |
| 658 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 238 | `e2e_happy_dependency_chain` | — | — | — | — | — | — | — | |
| 659 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 266 | `e2e_fail_and_retry` | — | — | — | — | — | — | — | |
| 660 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 347 | `e2e_cancel_mid_run` | — | — | — | — | — | — | — | |
| 661 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 381 | `e2e_blocked_and_resume` | — | — | — | — | — | — | — | |
| 662 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 454 | `e2e_double_force_tick_idempotent` | — | — | — | — | — | — | — | |
| 663 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/e2e_matrix.rs` | 499 | `e2e_cancel_before_materialize` | — | — | — | — | — | — | — | |
| 664 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/git_config_patch.rs` | 91 | `admin_patch_config_empty_url_clears_git_ref_without_url_and_garbage_url_are_400` | — | — | — | — | — | — | — | |
| 665 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 160 | `picker_prefers_idle_host_over_busier_host_listed_first` | — | — | — | — | — | — | — | |
| 666 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 213 | `picker_equal_load_prefers_oldest_heartbeat` | — | — | — | — | — | — | — | |
| 667 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 236 | `container_min_isolation_skips_process_only_host` | — | — | — | — | — | — | — | |
| 668 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 256 | `vm_min_isolation_skips_container_host` | — | — | — | — | — | — | — | |
| 669 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 276 | `container_need_picks_container_runtime_on_dual_host` | — | — | — | — | — | — | — | |
| 670 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 300 | `tick_must_not_enqueue_container_work_to_a_loopback_plane` | — | — | — | — | — | — | — | |
| 671 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 365 | `tick_must_not_assign_offbox_host_when_plane_url_is_loopback` | — | — | — | — | — | — | — | |
| 672 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 437 | `tick_may_assign_this_machine_host_when_plane_url_is_loopback` | — | — | — | — | — | — | — | |
| 673 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 476 | `tick_loopback_assigns_this_machine_not_offbox` | — | — | — | — | — | — | — | |
| 674 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_picker_product.rs` | 519 | `grok_plan_sets_guest_harness_even_when_plane_env_is_mock` | — | — | — | — | — | — | — | |
| 675 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 68 | `pick_uses_weakest_isolation_that_meets_process_need` | — | — | — | — | — | — | — | |
| 676 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 88 | `pick_skips_host_when_not_advertised_and_no_container` | — | — | — | — | — | — | — | |
| 677 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 94 | `pick_skips_full_host_and_disallowed` | — | — | — | — | — | — | — | |
| 678 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 109 | `pick_lowest_utilization_not_list_order` | — | — | — | — | — | — | — | |
| 679 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 128 | `pick_equal_load_prefers_oldest_heartbeat_not_newest_first` | — | — | — | — | — | — | — | |
| 680 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 143 | `pick_skips_when_advertised_cap_reached` | — | — | — | — | — | — | — | |
| 681 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 153 | `pick_at_advertised_cap_selects_other_native` | — | — | — | — | — | — | — | |
| 682 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 199 | `pick_advertised_zero_is_not_unlimited` | — | — | — | — | — | — | — | |
| 683 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 226 | `spawn_payload_shape_matches_contract` | — | — | — | — | — | — | — | |
| 684 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_pool_spawn.rs` | 253 | `remote_handle_roundtrips_host_id` | — | — | — | — | — | — | — | |
| 685 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_update_tick.rs` | 142 | `tick_holds_ready_when_only_host_is_restarting` | — | — | — | — | — | — | — | |
| 686 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_update_tick.rs` | 184 | `tick_offline_host_without_updating_leaves_ready` | — | — | — | — | — | — | — | |
| 687 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/host_update_tick.rs` | 245 | `tick_holds_ready_when_guest_killed_mid_swap_auto_update_on` | — | — | — | — | — | — | — | |
| 688 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 79 | `create_project_and_materialize_idempotent` | — | — | — | — | — | — | — | |
| 689 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 146 | `cancel_before_materialize_yields_zero_cards` | — | — | — | — | — | — | — | |
| 690 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 166 | `worker_report_inbox_idempotent_seq` | — | — | — | — | — | — | — | |
| 691 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 240 | `update_fleet_settings_ollama_and_rejects_unknown_as_off` | — | — | — | — | — | — | — | |
| 692 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 269 | `live_events_persist_and_duplicate_seq_is_noop` | — | — | — | — | — | — | — | |
| 693 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 358 | `replace_does_not_leave_sibling_or_accept_dead_heartbeat` | — | — | — | — | — | — | — | |
| 694 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/store_integration.rs` | 451 | `complete_spawn_failure_voids_claims_and_rejects_late_worker` | — | — | — | — | — | — | — | |
| 695 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/tick_integration.rs` | 146 | `tick_materializes_and_holds_ready_without_host` | — | — | — | — | — | — | — | |
| 696 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/tick_integration.rs` | 179 | `tick_host_assign_and_inbox_completes_chain` | — | — | — | — | — | — | — | |
| 697 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/tick_integration.rs` | 258 | `tick_cancel_before_materialize_terminates_run` | — | — | — | — | — | — | — | |
| 698 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/tick_integration.rs` | 286 | `tick_cancel_mid_run_drains_tasks` | — | — | — | — | — | — | — | |
| 699 | `rust-integration` | `default` | `crates/fleet-orchestrator/tests/tick_integration.rs` | 323 | `tick_inbox_status_success` | — | — | — | — | — | — | — | |

## fleet-runners-view

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 700 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1181 | `accept_jobs_copy_has_no_pause` | — | — | — | — | — | — | — | |
| 701 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1193 | `specs_line_is_iron_plus_load_ready` | — | — | — | — | — | — | — | |
| 702 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1219 | `specs_line_shows_intel_uhd` | — | — | — | — | — | — | — | |
| 703 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1237 | `specs_line_drops_missing_optionals` | — | — | — | — | — | — | — | |
| 704 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1248 | `hold_reason_says_running_jobs_finish` | — | — | — | — | — | — | — | |
| 705 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1282 | `never_paired_disabled_hidden_from_removed_list` | — | — | — | — | — | — | — | |
| 706 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1320 | `removed_clone_of_live_box_is_hidden` | — | — | — | — | — | — | — | |
| 707 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1345 | `older_removed_duplicate_is_hidden` | — | — | — | — | — | — | — | |
| 708 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1376 | `served_platforms_list_file_hashes_not_bake_id` | — | — | — | — | — | — | — | |
| 709 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1408 | `served_platforms_empty_inventory` | — | — | — | — | — | — | — | |
| 710 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1413 | `stale_package_platforms_flags_missing_and_old_versions` | — | — | — | — | — | — | — | |
| 711 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1457 | `stale_package_warning_none_when_every_os_is_current` | — | — | — | — | — | — | — | |
| 712 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1482 | `agent_status_phrase_matches_runners_and_chat` | — | — | — | — | — | — | — | |
| 713 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1497 | `ollama_catalog_label_is_private` | — | — | — | — | — | — | — | |
| 714 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1503 | `claude_aliases_are_not_version_pins` | — | — | — | — | — | — | — | |
| 715 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1511 | `chat_elapsed_formats` | — | — | — | — | — | — | — | |
| 716 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1528 | `chat_defaults_prefer_last_used_when_available` | — | — | — | — | — | — | — | |
| 717 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1541 | `chat_defaults_skip_last_used_when_not_ready` | — | — | — | — | — | — | — | |
| 718 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1550 | `chat_defaults_do_not_override_user_pick` | — | — | — | — | — | — | — | |
| 719 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1573 | `effort_picker_uses_last_used_when_model_still_lists_it` | — | — | — | — | — | — | — | |
| 720 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1587 | `effort_picker_falls_back_to_model_default` | — | — | — | — | — | — | — | |
| 721 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1606 | `effort_picker_hidden_when_model_has_no_menu` | — | — | — | — | — | — | — | |
| 722 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1616 | `software_never_reported_is_unknown` | — | — | — | — | — | — | — | |
| 723 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1621 | `software_current_shows_build_sha_and_os` | — | — | — | — | — | — | — | |
| 724 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1666 | `software_bin_rows_show_reported_semver_when_behind` | — | — | — | — | — | — | — | |
| 725 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1688 | `software_bin_rows_current_from_package_versions_without_desired` | — | — | — | — | — | — | — | |
| 726 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1706 | `software_bin_rows_same_version_different_hash_is_current` | — | — | — | — | — | — | — | |
| 727 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1731 | `software_bin_rows_flag_driver_mismatch` | — | — | — | — | — | — | — | |
| 728 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1752 | `software_matching_served_windows_package_is_up_to_date` | — | — | — | — | — | — | — | |
| 729 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1786 | `software_current_without_bake_id_is_up_to_date` | — | — | — | — | — | — | — | |
| 730 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1802 | `software_outdated_keeps_build_sha` | — | — | — | — | — | — | — | |
| 731 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1821 | `software_auto_update_does_not_fake_updating` | — | — | — | — | — | — | — | |
| 732 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1845 | `software_updating_beats_outdated` | — | — | — | — | — | — | — | |
| 733 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1855 | `offline_does_not_repeat_status_as_a_sentence` | — | — | — | — | — | — | — | |
| 734 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1863 | `control_plane_sorts_first` | — | — | — | — | — | — | — | |
| 735 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1880 | `extra_pills_show_pending_apply_and_outdated` | — | — | — | — | — | — | — | |
| 736 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1907 | `extra_pills_skip_control_plane` | — | — | — | — | — | — | — | |
| 737 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1920 | `status_label_maps_disabled_to_removed` | — | — | — | — | — | — | — | |
| 738 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1929 | `display_name_falls_back_when_blank` | — | — | — | — | — | — | — | |
| 739 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1939 | `display_name_tags_control_plane_and_this_machine` | — | — | — | — | — | — | — | |
| 740 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1950 | `format_bytes_and_disk_line` | — | — | — | — | — | — | — | |
| 741 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1966 | `provider_install_command_picks_os` | — | — | — | — | — | — | — | |
| 742 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1973 | `job_cap_is_min_of_plane_and_advertised` | — | — | — | — | — | — | — | |
| 743 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 1992 | `not_taking_reason_covers_pair_removed_error_hold` | — | — | — | — | — | — | — | |
| 744 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2032 | `software_title_matches_flag` | — | — | — | — | — | — | — | |
| 745 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2069 | `served_platforms_omits_bake_only_entries` | — | — | — | — | — | — | — | |
| 746 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2089 | `software_never_uses_file_hash_as_build_id` | — | — | — | — | — | — | — | |
| 747 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2100 | `relative_seen_none_is_never` | — | — | — | — | — | — | — | |
| 748 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2109 | `relative_seen_recent_is_just_now` | — | — | — | — | — | — | — | |
| 749 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2123 | `worker_id_label_prefers_machine_id_hex` | — | — | — | — | — | — | — | |
| 750 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2137 | `sort_hosts_plane_then_status_then_name` | — | — | — | — | — | — | — | |
| 751 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2177 | `shows_software_line_only_on_paired_boxes` | — | — | — | — | — | — | — | |
| 752 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2200 | `jobs_line_is_cap_plus_load` | — | — | — | — | — | — | — | |
| 753 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2227 | `plane_card_jobs_line_uses_inflight_over_max_concurrent` | — | — | — | — | — | — | — | |
| 754 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2281 | `last_seen_label_plane_is_this_process` | — | — | — | — | — | — | — | |
| 755 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2300 | `relative_seen_covers_second_hour_day_buckets` | — | — | — | — | — | — | — | |
| 756 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2322 | `format_ts_replaces_t_or_emdash` | — | — | — | — | — | — | — | |
| 757 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2328 | `hint_is_loopback_detects_localhost_and_ipv6` | — | — | — | — | — | — | — | |
| 758 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2336 | `install_commands_include_url_and_pair_token` | — | — | — | — | — | — | — | |
| 759 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2355 | `install_commands_prefer_server_hint` | — | — | — | — | — | — | — | |
| 760 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2373 | `host_patch_drain_serializes_when_set` | — | — | — | — | — | — | — | |
| 761 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2393 | `apply_patch_sets_and_clears_drain` | — | — | — | — | — | — | — | |
| 762 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2430 | `apply_patch_bumps_settings_pending_on_settings_change` | — | — | — | — | — | — | — | |
| 763 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2460 | `apply_patch_request_update_marks_host` | — | — | — | — | — | — | — | |
| 764 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2514 | `clipboard_helper_rejects_empty_payload` | — | — | — | — | — | — | — | |
| 765 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2526 | `identity_line_is_hostname_dot_platform` | — | — | — | — | — | — | — | |
| 766 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2548 | `settings_draft_dirty_ignores_plane_auto_update` | — | — | — | — | — | — | — | |
| 767 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2568 | `widgets_lock_while_pending_not_on_plane` | — | — | — | — | — | — | — | |
| 768 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2590 | `runtime_line_is_host_not_plane_backend` | — | — | — | — | — | — | — | |
| 769 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2599 | `draft_holding_and_clamp_max_jobs` | — | — | — | — | — | — | — | |
| 770 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2610 | `specs_line_formats_vram_and_multiple_gpus` | — | — | — | — | — | — | — | |
| 771 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2648 | `hint_is_loopback_does_not_flag_lan_or_public` | — | — | — | — | — | — | — | |
| 772 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2659 | `software_title_covers_bins_and_unknown` | — | — | — | — | — | — | — | |
| 773 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2699 | `needs_software_update_and_empty_package_report` | — | — | — | — | — | — | — | |
| 774 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2717 | `apply_patch_isolation_and_cap` | — | — | — | — | — | — | — | |
| 775 | `unit` | `default` | `crates/fleet-runners-view/src/lib.rs` | 2739 | `windows_install_does_not_hardcode_arch` | — | — | — | — | — | — | — | |
| 776 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 82 | `cloud_has_four_named_cards` | — | — | — | — | — | — | — | |
| 777 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 98 | `every_cloud_card_is_coming_soon` | — | — | — | — | — | — | — | |
| 778 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 107 | `cloud_card_bodies_are_placeholders_not_actions` | — | — | — | — | — | — | — | |
| 779 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 138 | `partners_coming_soon_and_trust_copy` | — | — | — | — | — | — | — | |
| 780 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 163 | `disclaimer_defaults_off` | — | — | — | — | — | — | — | |
| 781 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 175 | `ack_does_not_enable_lend_or_assignable` | — | — | — | — | — | — | — | |
| 782 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 189 | `cloud_page_live_source_stays_coming_soon_placeholders` | — | — | — | — | — | — | — | |
| 783 | `unit` | `default` | `crates/fleet-runners-view/src/placeholders.rs` | 223 | `partners_page_live_source_stays_coming_soon_and_ack_is_not_lend` | — | — | — | — | — | — | — | |
| 784 | `unit` | `default` | `crates/fleet-runners-view/src/routes.rs` | 73 | `nav_children_labels_and_paths` | — | — | — | — | — | — | — | |
| 785 | `unit` | `default` | `crates/fleet-runners-view/src/routes.rs` | 119 | `runners_index_redirects_to_self_hosted` | — | — | — | — | — | — | — | |
| 786 | `unit` | `default` | `crates/fleet-runners-view/src/routes.rs` | 143 | `workers_redirects_to_self_hosted` | — | — | — | — | — | — | — | |
| 787 | `unit` | `default` | `crates/fleet-runners-view/src/routes.rs` | 167 | `live_ui_routes_redirect_to_self_hosted_page` | — | — | — | — | — | — | — | |

## fleet-sandbox-driver

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 788 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 665 | `msg_id_parses_int_and_string` | — | — | — | — | — | — | — | |
| 789 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 672 | `chunk_text_agent_message` | — | — | — | — | — | — | — | |
| 790 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 681 | `chunk_text_skips_user_and_empty` | — | — | — | — | — | — | — | |
| 791 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 693 | `format_session_update_includes_tool_calls` | — | — | — | — | — | — | — | |
| 792 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 712 | `pick_auth_prefers_cached_token` | — | — | — | — | — | — | — | |
| 793 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 723 | `pick_auth_skips_login_only` | — | — | — | — | — | — | — | |
| 794 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 729 | `run_session_against_mock_agent` | — | — | — | — | — | — | — | |
| 795 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 787 | `permission_default_deny` | — | — | — | — | — | — | — | |
| 796 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 792 | `permission_star_and_substring` | — | — | — | — | — | — | — | |
| 797 | `unit` | `default` | `crates/fleet-sandbox-driver/src/acp.rs` | 808 | `pick_permission_prefers_once_never_always` | — | — | — | — | — | — | — | |
| 798 | `unit` | `default` | `crates/fleet-sandbox-driver/src/handshake.rs` | 187 | `canonical_matches_python_style` | — | — | — | — | — | — | — | |
| 799 | `unit` | `default` | `crates/fleet-sandbox-driver/src/handshake.rs` | 193 | `sign_and_verify_roundtrip` | — | — | — | — | — | — | — | |
| 800 | `unit` | `default` | `crates/fleet-sandbox-driver/src/handshake.rs` | 214 | `init_from_env_strips_environ_keeps_secret` | — | — | — | — | — | — | — | |
| 801 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 196 | `custom_harness_requires_command` | — | — | — | — | — | — | — | |
| 802 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 202 | `custom_harness_ok` | — | — | — | — | — | — | — | |
| 803 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 217 | `unknown_harness_errors` | — | — | — | — | — | — | — | |
| 804 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 224 | `pi_harness_resolves_when_binary_or_npx` | — | — | — | — | — | — | — | |
| 805 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 247 | `opencode_spec_when_binary_present_or_skip` | — | — | — | — | — | — | — | |
| 806 | `unit` | `default` | `crates/fleet-sandbox-driver/src/harness.rs` | 266 | `opencode_command_override_still_checks_default_bin_first` | — | — | — | — | — | — | — | |
| 807 | `unit` | `default` | `crates/fleet-sandbox-driver/src/job.rs` | 201 | `deserializes_minimal_job` | — | — | — | — | — | — | — | |
| 808 | `unit` | `default` | `crates/fleet-sandbox-driver/src/job.rs` | 215 | `deserializes_custom_harness_and_provider` | — | — | — | — | — | — | — | |
| 809 | `unit` | `default` | `crates/fleet-sandbox-driver/src/job.rs` | 238 | `result_serializes_ok_shape` | — | — | — | — | — | — | — | |
| 810 | `unit` | `default` | `crates/fleet-sandbox-driver/src/job.rs` | 266 | `deserializes_project_setup` | — | — | — | — | — | — | — | |
| 811 | `unit` | `default` | `crates/fleet-sandbox-driver/src/job.rs` | 284 | `deserializes_workspace_tools` | — | — | — | — | — | — | — | |
| 812 | `unit` | `default` | `crates/fleet-sandbox-driver/src/lib.rs` | 26 | `parse_env_bool_truthy` | — | — | — | — | — | — | — | |
| 813 | `unit` | `default` | `crates/fleet-sandbox-driver/src/lib.rs` | 33 | `parse_env_bool_falsy` | — | — | — | — | — | — | — | |
| 814 | `unit` | `default` | `crates/fleet-sandbox-driver/src/lib.rs` | 40 | `parse_env_bool_invalid` | — | — | — | — | — | — | — | |
| 815 | `unit` | `default` | `crates/fleet-sandbox-driver/src/opencode_config.rs` | 404 | `full_opencode_object_passthrough` | — | — | — | — | — | — | — | |
| 816 | `unit` | `default` | `crates/fleet-sandbox-driver/src/opencode_config.rs` | 422 | `provider_spec_builds_openai_compatible_shape` | — | — | — | — | — | — | — | |
| 817 | `unit` | `default` | `crates/fleet-sandbox-driver/src/opencode_config.rs` | 463 | `model_without_prefix_gets_provider_prefix` | — | — | — | — | — | — | — | |
| 818 | `unit` | `default` | `crates/fleet-sandbox-driver/src/opencode_config.rs` | 487 | `job_provider_field_used_when_no_opencode_object` | — | — | — | — | — | — | — | |
| 819 | `unit` | `default` | `crates/fleet-sandbox-driver/src/pi_config.rs` | 211 | `builds_lmstudio_shape` | — | — | — | — | — | — | — | |
| 820 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 238 | `default_work_dir_is_per_job` | — | — | — | — | — | — | — | |
| 821 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 247 | `jobs_root_default_is_workspace_jobs` | — | — | — | — | — | — | — | |
| 822 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 257 | `jobs_root_from_explicit_path` | — | — | — | — | — | — | — | |
| 823 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 265 | `resolve_work_dir_explicit_and_default` | — | — | — | — | — | — | — | |
| 824 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 277 | `two_jobs_do_not_share_default_worktree` | — | — | — | — | — | — | — | |
| 825 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 286 | `validate_job_cwd_accepts_jobs_tree` | — | — | — | — | — | — | — | |
| 826 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 292 | `validate_job_cwd_rejects_escape_and_outside` | — | — | — | — | — | — | — | |
| 827 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 299 | `resolve_named_tool_ok_and_unknown` | — | — | — | — | — | — | — | |
| 828 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 315 | `resolve_named_tool_rejects_metachar_argv` | — | — | — | — | — | — | — | |
| 829 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 340 | `raw_exec_flag` | — | — | — | — | — | — | — | |
| 830 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 348 | `raw_exec_default_off_unless_fleet_allow_raw_exec` | — | — | — | — | — | — | — | |
| 831 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 372 | `permission_default_deny_and_star` | — | — | — | — | — | — | — | |
| 832 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 389 | `permission_file_edit_requires_allowed_folder` | — | — | — | — | — | — | — | |
| 833 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 423 | `bash_substring_in_filename_is_not_unrestricted_shell` | — | — | — | — | — | — | — | |
| 834 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 437 | `bash_in_allowlist_cannot_write_outside_paths` | — | — | — | — | — | — | — | |
| 835 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 457 | `validate_tool_bin_rejects_curl_and_bash` | — | — | — | — | — | — | — | |
| 836 | `unit` | `default` | `crates/fleet-sandbox-driver/src/policy.rs` | 466 | `django_and_node_setup_shapes` | — | — | — | — | — | — | — | |
| 837 | `unit` | `default` | `crates/fleet-sandbox-driver/src/provision.rs` | 501 | `setup_commands_from_project_setup` | — | — | — | — | — | — | — | |
| 838 | `unit` | `default` | `crates/fleet-sandbox-driver/src/provision.rs` | 516 | `setup_commands_script_strategy` | — | — | — | — | — | — | — | |
| 839 | `unit` | `default` | `crates/fleet-sandbox-driver/src/provision.rs` | 531 | `accept_agent_work_dir_keeps_requested_when_ok` | — | — | — | — | — | — | — | |
| 840 | `unit` | `default` | `crates/fleet-sandbox-driver/src/provision.rs` | 541 | `accept_agent_work_dir_rejects_escape` | — | — | — | — | — | — | — | |
| 841 | `unit` | `default` | `crates/fleet-sandbox-driver/src/provision.rs` | 548 | `write_tools_file_always_and_rejects_bad` | — | — | — | — | — | — | — | |
| 842 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 17 | `invoices_job_is_default_deny_with_named_tools` | — | — | — | — | — | — | — | |
| 843 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 78 | `invoices_opencode_and_default_match_marker_task` | — | — | — | — | — | — | — | |
| 844 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 97 | `invoices_pi_job_is_model_field_task` | — | — | — | — | — | — | — | |
| 845 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 130 | `minimal_node_job_shape_differs_from_django` | — | — | — | — | — | — | — | |
| 846 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 156 | `deny_default_fixture_empty_allow` | — | — | — | — | — | — | — | |
| 847 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 163 | `both_setup_shapes_have_strategy_commands` | — | — | — | — | — | — | — | |
| 848 | `rust-integration` | `default` | `crates/fleet-sandbox-driver/tests/job_fixtures.rs` | 178 | `commands_only_shape_empty_tools_default_deny` | — | — | — | — | — | — | — | |

## fleet-server

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 849 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1562 | `transition_matrix_basics` | — | — | — | — | — | — | — | |
| 850 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1572 | `acting_can_leave_to_every_non_self_non_impossible` | — | — | — | — | — | — | — | |
| 851 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1585 | `terminals_have_no_outbound_except_same_status` | — | — | — | — | — | — | — | |
| 852 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1608 | `parse_and_terminal_flags` | — | — | — | — | — | — | — | |
| 853 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1631 | `acting_can_transition_to_completed_failed_cancelled_waiting_deciding_open_and_self` | — | — | — | — | — | — | — | |
| 854 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1642 | `terminal_completed_failed_cancelled_cannot_leave_same_status_ok` | — | — | — | — | — | — | — | |
| 855 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1665 | `parse_unknown_and_blank_none_all_seven_wires_some` | — | — | — | — | — | — | — | |
| 856 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1685 | `is_terminal_only_completed_failed_cancelled` | — | — | — | — | — | — | — | |
| 857 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1696 | `parse_decision_strips_fences` | — | — | — | — | — | — | — | |
| 858 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1704 | `heuristic_waits_without_facts` | — | — | — | — | — | — | — | |
| 859 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1728 | `heuristic_completes_on_flag` | — | — | — | — | — | — | — | |
| 860 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1752 | `observations_are_capped` | — | — | — | — | — | — | — | |
| 861 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1767 | `parse_tool_request_requires_provider_and_op` | — | — | — | — | — | — | — | |
| 862 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1781 | `llm_decide_wanted_respects_env_and_providers` | — | — | — | — | — | — | — | |
| 863 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1811 | `case_auto_decide_off_0_false_does_not_apply_decide` | — | — | — | — | — | — | — | |
| 864 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1833 | `case_auto_decide_off_unclaim_does_not_bump_decision_count` | — | — | — | — | — | — | — | |
| 865 | `unit` | `default` | `crates/fleet-server/src/cases.rs` | 1850 | `case_auto_decide_off_failed_unclaim_does_not_leave_deciding` | — | — | — | — | — | — | — | |
| 866 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1089 | `chat_tool_args_preserve_workspace_id_and_foreign_is_forbidden` | — | — | — | — | — | — | — | |
| 867 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1110 | `chat_tool_session_member_cannot_target_another_workspace` | — | — | — | — | — | — | — | |
| 868 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1129 | `chat_tool_unparseable_workspace_id_is_forbidden` | — | — | — | — | — | — | — | |
| 869 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1149 | `claude_logged_in_without_models_is_not_logged_out` | — | — | — | — | — | — | — | |
| 870 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1167 | `ollama_status_label_is_private` | — | — | — | — | — | — | — | |
| 871 | `unit` | `default` | `crates/fleet-server/src/chat.rs` | 1185 | `public_base_url_strips_userinfo` | — | — | — | — | — | — | — | |
| 872 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2822 | `stamp_plan_llm_writes_provider_and_model` | — | — | — | — | — | — | — | |
| 873 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2838 | `parse_launch_plan_rejects_empty_and_blank_ids` | — | — | — | — | — | — | — | |
| 874 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2864 | `build_chat_plan_sets_chat_source_and_single_task` | — | — | — | — | — | — | — | |
| 875 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2876 | `parse_launch_args_rejects_empty_title` | — | — | — | — | — | — | — | |
| 876 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2882 | `parse_launch_args_from_string` | — | — | — | — | — | — | — | |
| 877 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2890 | `chat_plan_cap_matches_orchestrator` | — | — | — | — | — | — | — | |
| 878 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2896 | `tool_definitions_include_launch_and_integrations` | — | — | — | — | — | — | — | |
| 879 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2926 | `coerce_fills_notion_text_from_slack_history` | — | — | — | — | — | — | — | |
| 880 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2952 | `coerce_resolves_hash_eng_from_listed_channels` | — | — | — | — | — | — | — | |
| 881 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2975 | `retry_hint_names_required_text` | — | — | — | — | — | — | — | |
| 882 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 2986 | `continue_rewrite_does_not_inject_ship_playbook` | — | — | — | — | — | — | — | |
| 883 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 3008 | `chat_plan_is_not_rewritten_into_a_pr_pipeline` | — | — | — | — | — | — | — | |
| 884 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 3021 | `connector_catalog_lists_every_provider_and_ops` | — | — | — | — | — | — | — | |
| 885 | `unit` | `default` | `crates/fleet-server/src/chat_tools.rs` | 3034 | `multi_task_plan_validates` | — | — | — | — | — | — | — | |
| 886 | `unit` | `default` | `crates/fleet-server/src/config.rs` | 192 | `database_url_unset_empty_and_whitespace_fails_from_env` | — | — | — | — | — | — | — | |
| 887 | `unit` | `default` | `crates/fleet-server/src/config.rs` | 206 | `database_url_explicit_still_parses_bind_cors_auth` | — | — | — | — | — | — | — | |
| 888 | `unit` | `default` | `crates/fleet-server/src/config.rs` | 243 | `unset_worker_dist_dir_parses_as_none` | — | — | — | — | — | — | — | |
| 889 | `unit` | `default` | `crates/fleet-server/src/config.rs` | 248 | `empty_or_whitespace_worker_dist_dir_parses_as_none` | — | — | — | — | — | — | — | |
| 890 | `unit` | `default` | `crates/fleet-server/src/config.rs` | 255 | `set_worker_dist_dir_parses_trimmed_path` | — | — | — | — | — | — | — | |
| 891 | `unit` | `default` | `crates/fleet-server/src/fleet_routes.rs` | 993 | `patch_body_omitted_url_is_none_empty_string_is_some` | — | — | — | — | — | — | — | |
| 892 | `unit` | `default` | `crates/fleet-server/src/fleet_routes.rs` | 1007 | `cookie_patch_uses_shared_apply_git_remote_patch` | — | — | — | — | — | — | — | |
| 893 | `unit` | `default` | `crates/fleet-server/src/fleet_routes.rs` | 1028 | `cookie_patch_empty_url_clears_git_ref_without_url_and_garbage_url_are_errors` | — | — | — | — | — | — | — | |
| 894 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1227 | `parse_grok_models_lists_46_first` | — | — | — | — | — | — | — | |
| 895 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1236 | `put_effort_accepts_provider_values` | — | — | — | — | — | — | — | |
| 896 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1246 | `default_auth_under_home_grok` | — | — | — | — | — | — | — | |
| 897 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1252 | `expand_tilde` | — | — | — | — | — | — | — | |
| 898 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1261 | `parse_expires_rfc3339` | — | — | — | — | — | — | — | |
| 899 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1291 | `missing_or_empty_api_key_fails_closed_without_hang` | — | — | — | — | — | — | — | |
| 900 | `unit` | `default` | `crates/fleet-server/src/grok.rs` | 1323 | `provider_status_without_key_unavailable` | — | — | — | — | — | — | — | |
| 901 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 359 | `follow_connected_picks_up_new_apps` | — | — | — | — | — | — | — | |
| 902 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 371 | `pinned_allowlist_stays_pinned` | — | — | — | — | — | — | — | |
| 903 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 381 | `empty_allowlist_without_follow_watches_nothing` | — | — | — | — | — | — | — | |
| 904 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 387 | `drops_non_inbox_and_disconnected` | — | — | — | — | — | — | — | |
| 905 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 397 | `merge_legacy_fields_into_filters` | — | — | — | — | — | — | — | |
| 906 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 430 | `pinned_webhook_rejects_other_providers` | — | — | — | — | — | — | — | |
| 907 | `unit` | `default` | `crates/fleet-server/src/inbox_sources.rs` | 447 | `trello_board_allowlist_is_exact` | — | — | — | — | — | — | — | |
| 908 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 851 | `parses_github_issue_opened` | — | — | — | — | — | — | — | |
| 909 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 871 | `github_hmac_accepts_valid_and_rejects_bad` | — | — | — | — | — | — | — | |
| 910 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 879 | `slack_url_verification_is_challenge` | — | — | — | — | — | — | — | |
| 911 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 888 | `parses_jira_and_linear` | — | — | — | — | — | — | — | |
| 912 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 921 | `github_issue_comment_is_not_parsed_as_issue_open` | — | — | — | — | — | — | — | |
| 913 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 941 | `slack_rejects_stale_timestamp` | — | — | — | — | — | — | — | |
| 914 | `unit` | `default` | `crates/fleet-server/src/inbox_webhooks.rs` | 955 | `gitlab_without_project_or_kind_is_ignored` | — | — | — | — | — | — | — | |
| 915 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 460 | `manifest_json_is_served_as_json` | — | — | — | — | — | — | — | |
| 916 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 473 | `map_uname_linux_x86_64` | — | — | — | — | — | — | — | |
| 917 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 496 | `script_body_serves_linux_armv7_musl` | — | — | — | — | — | — | — | |
| 918 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 513 | `script_body_names_host_and_driver` | — | — | — | — | — | — | — | |
| 919 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 538 | `script_body_windows_arm64_fetches_libunwind` | — | — | — | — | — | — | — | |
| 920 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 555 | `native_platform_id_is_a_single_token` | — | — | — | — | — | — | — | |
| 921 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 563 | `resolve_prefers_dist_dir` | — | — | — | — | — | — | — | |
| 922 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 580 | `resolve_exe_platform_then_native_sibling` | — | — | — | — | — | — | — | |
| 923 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 622 | `resolve_workspace_target_release_then_debug_when_native` | — | — | — | — | — | — | — | |
| 924 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 652 | `resolve_rejects_control_plane_and_unknown_even_when_planted` | — | — | — | — | — | — | — | |
| 925 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 701 | `worker_dist_unset_resolve_is_none` | — | — | — | — | — | — | — | |
| 926 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 711 | `worker_dist_missing_bin_hint_includes_build_from_source` | — | — | — | — | — | — | — | |
| 927 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 735 | `worker_dist_script_body_independent_of_dist` | — | — | — | — | — | — | — | |
| 928 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 837 | `install_scripts_use_worker_public_base_not_api_public_base` | — | — | — | — | — | — | — | |
| 929 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 923 | `install_worker_sh_exports_fleet_pair_token_and_fleet_url` | — | — | — | — | — | — | — | |
| 930 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 955 | `install_worker_unix_hits_worker_public_base_not_api_public_base` | — | — | — | — | — | — | — | |
| 931 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1056 | `install_worker_unix_common_requires_pair_token_before_writing_bins` | — | — | — | — | — | — | — | |
| 932 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1172 | `install_worker_missing_pair_token_fails_closed_before_writing_bins` | — | — | — | — | — | — | — | |
| 933 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1177 | `install_worker_empty_pair_token_fails_closed_before_writing_bins` | — | — | — | — | — | — | — | |
| 934 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1182 | `install_worker_whitespace_pair_token_fails_closed_before_writing_bins` | — | — | — | — | — | — | — | |
| 935 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1189 | `install_worker_present_pair_token_passes_token_gate` | — | — | — | — | — | — | — | |
| 936 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1253 | `install_worker_ps1_selects_machine_arch_and_requires_pair_token` | — | — | — | — | — | — | — | |
| 937 | `unit` | `default` | `crates/fleet-server/src/install_routes.rs` | 1529 | `install_loopback_url_warns_and_offbox_pair_fails_closed` | — | — | — | — | — | — | — | |
| 938 | `unit` | `default` | `crates/fleet-server/src/local_models.rs` | 384 | `parse_library_keeps_popular_order_and_caps` | — | — | — | — | — | — | — | |
| 939 | `unit` | `default` | `crates/fleet-server/src/local_models.rs` | 404 | `parse_tags_reads_size_and_latest_badge` | — | — | — | — | — | — | — | |
| 940 | `unit` | `default` | `crates/fleet-server/src/local_models.rs` | 427 | `slug_rejects_paths` | — | — | — | — | — | — | — | |
| 941 | `unit` | `default` | `crates/fleet-server/src/local_models.rs` | 672 | `disabled_does_not_proxy_to_lmstudio_1234` | — | — | — | — | — | — | — | |
| 942 | `unit` | `default` | `crates/fleet-server/src/merge_gate.rs` | 8 | `merge_to_main_requires_green_tests` | — | — | — | — | — | — | — | |
| 943 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 193 | `toml_has_semver_for_both_bins` | — | — | — | — | — | — | — | |
| 944 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 222 | `verify_is_not_behind_package_versions` | — | — | — | — | — | — | — | |
| 945 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 238 | `version_is_ahead_compares_semver` | — | — | — | — | — | — | — | |
| 946 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 250 | `stale_warning_names_required_os_missing_current_host_or_driver` | — | — | — | — | — | — | — | |
| 947 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 278 | `stale_warning_when_host_current_and_driver_old` | — | — | — | — | — | — | — | |
| 948 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 291 | `stale_warning_none_when_every_required_os_is_current` | — | — | — | — | — | — | — | |
| 949 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 342 | `verify_versions_are_not_plane_bake_or_fleet_git_sha` | — | — | — | — | — | — | — | |
| 950 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 363 | `version_helpers_do_not_treat_git_sha_as_verify_semver` | — | — | — | — | — | — | — | |
| 951 | `unit` | `default` | `crates/fleet-server/src/package_versions.rs` | 397 | `stale_helpers_do_not_treat_git_sha_as_verify_semver` | — | — | — | — | — | — | — | |
| 952 | `unit` | `default` | `crates/fleet-server/src/partners.rs` | 101 | `disclaimer_ack_is_timestamp_only` | — | — | — | — | — | — | — | |
| 953 | `unit` | `default` | `crates/fleet-server/src/partners.rs` | 137 | `disclaimer_ack_first_write_wins_echoes_stored_timestamp` | — | — | — | — | — | — | — | |
| 954 | `unit` | `default` | `crates/fleet-server/src/partners.rs` | 161 | `disclaimer_ack_does_not_enable_lend_enqueue_or_assignable` | — | — | — | — | — | — | — | |
| 955 | `unit` | `default` | `crates/fleet-server/src/partners.rs` | 178 | `disclaimer_handlers_have_no_assignable_or_lend_path` | — | — | — | — | — | — | — | |
| 956 | `unit` | `default` | `crates/fleet-server/src/providers.rs` | 793 | `openai_status_without_key` | — | — | — | — | — | — | — | |
| 957 | `unit` | `default` | `crates/fleet-server/src/providers.rs` | 806 | `openai_status_disabled_is_coming_soon` | — | — | — | — | — | — | — | |
| 958 | `unit` | `default` | `crates/fleet-server/src/providers.rs` | 819 | `claude_status_without_key` | — | — | — | — | — | — | — | |
| 959 | `unit` | `default` | `crates/fleet-server/src/providers.rs` | 834 | `unknown_empty_typo_provider_fails_closed` | — | — | — | — | — | — | — | |
| 960 | `unit` | `default` | `crates/fleet-server/src/providers.rs` | 959 | `chat_complete_tool_rounds_capped` | — | — | — | — | — | — | — | |
| 961 | `unit` | `default` | `crates/fleet-server/src/retry.rs` | 118 | `provider_retries_network_and_429` | — | — | — | — | — | — | — | |
| 962 | `unit` | `default` | `crates/fleet-server/src/retry.rs` | 131 | `tool_retries_upstream_not_bad_args` | — | — | — | — | — | — | — | |
| 963 | `unit` | `default` | `crates/fleet-server/src/retry.rs` | 175 | `retry_backoff_first_attempts_are_400ms_steps` | — | — | — | — | — | — | — | |
| 964 | `unit` | `default` | `crates/fleet-server/src/retry.rs` | 181 | `retry_backoff_zero_attempt_does_not_busy_spin` | — | — | — | — | — | — | — | |
| 965 | `unit` | `default` | `crates/fleet-server/src/retry.rs` | 190 | `retry_backoff_huge_attempt_clamps_without_overflow` | — | — | — | — | — | — | — | |
| 966 | `unit` | `default` | `crates/fleet-server/src/runner_chat.rs` | 258 | `transcript_labels_roles` | — | — | — | — | — | — | — | |
| 967 | `unit` | `default` | `crates/fleet-server/src/runner_chat.rs` | 270 | `empty_messages_rejected_without_hang` | — | — | — | — | — | — | — | |
| 968 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2218 | `parses_linear_and_jira_keys` | — | — | — | — | — | — | — | |
| 969 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2228 | `parses_github_hash_and_url` | — | — | — | — | — | — | — | |
| 970 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2239 | `inbox_finish_plan_is_work_then_review` | — | — | — | — | — | — | — | |
| 971 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2254 | `inbox_message_and_page_are_single_ops_tasks` | — | — | — | — | — | — | — | |
| 972 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2280 | `parses_slack_and_notion_keys` | — | — | — | — | — | — | — | |
| 973 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2292 | `status_done_does_not_close_incomplete_or_not_done` | — | — | — | — | — | — | — | |
| 974 | `unit` | `default` | `crates/fleet-server/src/tickets.rs` | 2321 | `status_done_does_not_close_not_completed_not_closed_or_not_resolved` | — | — | — | — | — | — | — | |
| 975 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1347 | `matching_sha_is_plane_bake` | — | — | — | — | — | — | — | |
| 976 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1354 | `missing_or_different_sha_is_not_plane_bake` | — | — | — | — | — | — | — | |
| 977 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1436 | `this_machine_behind_must_receive_desired` | — | — | — | — | — | — | — | |
| 978 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1456 | `this_machine_on_current_version_withholds_tarball` | — | — | — | — | — | — | — | |
| 979 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1480 | `this_machine_without_report_must_receive_desired` | — | — | — | — | — | — | — | |
| 980 | `unit` | `default` | `crates/fleet-server/src/worker_hosts.rs` | 1487 | `host_on_this_process_bake_withholds_even_when_remote` | — | — | — | — | — | — | — | |
| 981 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 921 | `chat_stream_other_workspace_forbidden` | — | — | — | — | — | — | — | |
| 982 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 983 | `chat_stream_conversation_user_scope` | — | — | — | — | — | — | — | |
| 983 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1233 | `chat_stream_deleted_conversation_stays_gone` | — | — | — | — | — | — | — | |
| 984 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1367 | `chat_stream_empty_messages_bad_request` | — | — | — | — | — | — | — | |
| 985 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1400 | `chat_invalid_role_stream_rejected_no_persist` | — | — | — | — | — | — | — | |
| 986 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1638 | `chat_message_max_32k_stream_rejected_no_persist` | — | — | — | — | — | — | — | |
| 987 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1888 | `chat_stream_disabled_providers_fail_closed_quickly` | — | — | — | — | — | — | — | |
| 988 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 1926 | `chat_status_complete_stream_other_workspace_cookie_forbidden_before_enqueue` | — | — | — | — | — | — | — | |
| 989 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 2032 | `chat_foreign_runner_id_is_404_and_does_not_enqueue` | — | — | — | — | — | — | — | |
| 990 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 2327 | `chat_complete_and_stream_empty_messages_rejected_before_runner_wait` | — | — | — | — | — | — | — | |
| 991 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 2459 | `chat_revoked_runner_no_enqueue` | — | — | — | — | — | — | — | |
| 992 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 2754 | `chat_at_job_cap_no_enqueue` | — | — | — | — | — | — | — | |
| 993 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 2907 | `chat_updating_host_no_enqueue` | — | — | — | — | — | — | — | |
| 994 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 3309 | `chat_pending_offline_held_runner_no_enqueue` | — | — | — | — | — | — | — | |
| 995 | `rust-integration` | `default` | `crates/fleet-server/tests/chat_stream_http.rs` | 3710 | `chat_effective_host_false_no_enqueue` | — | — | — | — | — | — | — | |
| 996 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 636 | `health_ok_healthz_ok_api_index_lists_auth_and_fleet` | — | — | — | — | — | — | — | |
| 997 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 705 | `signup_login_fleet_project_id_matches_workspace_owner` | — | — | — | — | — | — | — | |
| 998 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 724 | `fixture_bff_start_and_tick_with_cookie` | — | — | — | — | — | — | — | |
| 999 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 761 | `resume_non_blocked_task_is_conflict_and_unknown_kill_is_404` | — | — | — | — | — | — | — | |
| 1000 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 803 | `cancel_run_reaches_cancelled` | — | — | — | — | — | — | — | |
| 1001 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 851 | `patch_fleet_config_round_trip_empty_clears_git_ref_only_400` | — | — | — | — | — | — | — | |
| 1002 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 918 | `workers_sandboxes_match_sessions_and_detail_shape` | — | — | — | — | — | — | — | |
| 1003 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1004 | `personas_seed_starters_create_patch_archive` | — | — | — | — | — | — | — | |
| 1004 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1088 | `integrations_catalog_oauth_start_delete_and_callback_contracts` | — | — | — | — | — | — | — | |
| 1005 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1392 | `chat_status_create_list_delete_other_user_forbidden` | — | — | — | — | — | — | — | |
| 1006 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1538 | `chat_tool_launch_agent_task_waits_for_host_then_inbox` | — | — | — | — | — | — | — | |
| 1007 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1643 | `v1_projects_rejects_missing_bearer_and_session_cookie` | — | — | — | — | — | — | — | |
| 1008 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1679 | `fleet_requires_membership` | — | — | — | — | — | — | — | |
| 1009 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1723 | `fleet_settings_get_patch_and_rejects_grok_store` | — | — | — | — | — | — | — | |
| 1010 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1786 | `fleet_personas_lists_builtin_or_roster` | — | — | — | — | — | — | — | |
| 1011 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1811 | `chat_tool_list_agent_personas_and_launch_plan_stamps_llm` | — | — | — | — | — | — | — | |
| 1012 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1883 | `chat_tools_human_can_talk_on_team_channel` | — | — | — | — | — | — | — | |
| 1013 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1955 | `chat_tools_ticket_inbox_is_available` | — | — | — | — | — | — | — | |
| 1014 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 1991 | `cases_create_list_observe_use_tool_and_cancel` | — | — | — | — | — | — | — | |
| 1015 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2113 | `partners_disclaimer_get_defaults_not_accepted` | — | — | — | — | — | — | — | |
| 1016 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2132 | `partners_disclaimer_post_then_get_ack_is_sticky` | — | — | — | — | — | — | — | |
| 1017 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2185 | `partners_disclaimer_ack_does_not_enqueue_or_create_worker_hosts` | — | — | — | — | — | — | — | |
| 1018 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2380 | `cases_other_workspace_cannot_get_observe_decide_cancel_wake` | — | — | — | — | — | — | — | |
| 1019 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2466 | `cases_cancel_is_terminal_and_idempotent` | — | — | — | — | — | — | — | |
| 1020 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2525 | `cases_observe_empty_note_is_required` | — | — | — | — | — | — | — | |
| 1021 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2555 | `cases_observe_too_long` | — | — | — | — | — | — | — | |
| 1022 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2603 | `cases_wait_seconds_zero_clamps_to_min` | — | — | — | — | — | — | — | |
| 1023 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2643 | `cases_wait_seconds_huge_clamps_to_max` | — | — | — | — | — | — | — | |
| 1024 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2695 | `cases_wake_owner_ok_other_workspace_forbidden` | — | — | — | — | — | — | — | |
| 1025 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2739 | `cases_decide_unknown_action_waits_with_last_error` | — | — | — | — | — | — | — | |
| 1026 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2765 | `cases_list_status_filter_open_excludes_cancelled` | — | — | — | — | — | — | — | |
| 1027 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2838 | `inbox_other_workspace_forbidden` | — | — | — | — | — | — | — | |
| 1028 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2868 | `inbox_finish_missing_or_unknown_provider_rejected` | — | — | — | — | — | — | — | |
| 1029 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2908 | `inbox_poll_without_monitor_is_not_500` | — | — | — | — | — | — | — | |
| 1030 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2935 | `inbox_unauthenticated_get_is_401` | — | — | — | — | — | — | — | |
| 1031 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 2944 | `inbox_monitor_defaults_auto_launch_and_preserves_filters` | — | — | — | — | — | — | — | |
| 1032 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 3014 | `inbox_monitor_follow_connected_watches_whatever_is_connected` | — | — | — | — | — | — | — | |
| 1033 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 3077 | `inbox_webhook_token_auth_and_pinned_provider` | — | — | — | — | — | — | — | |
| 1034 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 3155 | `workforce_goal_survives_and_slack_post_runs_directly` | — | — | — | — | — | — | — | |
| 1035 | `rust-integration` | `default` | `crates/fleet-server/tests/http_api.rs` | 3211 | `session_live_comes_from_worker_ingest_not_plane_file` | — | — | — | — | — | — | — | |
| 1036 | `rust-integration` | `default` | `crates/fleet-server/tests/inbox_state_query.rs` | 470 | `inbox_get_state_query_filters_and_unknown_does_not_500` | — | — | — | — | — | — | — | |
| 1037 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 130 | `t1_list_and_get_connected_apps` | — | — | — | — | — | — | — | |
| 1038 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 164 | `t2_slack_list_channels_and_users` | — | — | — | — | — | — | — | |
| 1039 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 182 | `t3_notion_search` | — | — | — | — | — | — | — | |
| 1040 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 202 | `t4_github_list_repos` | — | — | — | — | — | — | — | |
| 1041 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 237 | `t5_jira_not_connected` | — | — | — | — | — | — | — | |
| 1042 | `rust-integration` | `ignored` | `crates/fleet-server/tests/integrations_live.rs` | 268 | `l1_grok_chat_calls_slack_integration_query` | — | — | — | — | — | — | — | |
| 1043 | `rust-integration` | `default` | `crates/fleet-server/tests/settings_http.rs` | 406 | `fleet_settings_min_isolation_default_process_patch_container_rejects_native_host_k8s` | — | — | — | — | — | — | — | |
| 1044 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 475 | `host_mode_job_heartbeat_promotes_session_healthy` | — | — | — | — | — | — | — | |
| 1045 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 717 | `tick_assigns_remote_host_instead_of_local_stub` | — | — | — | — | — | — | — | |
| 1046 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 804 | `tick_without_host_leaves_ready` | — | — | — | — | — | — | — | |
| 1047 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 864 | `complete_ok_false_is_spawn_failure_not_success` | — | — | — | — | — | — | — | |
| 1048 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 985 | `signal_session_complete_does_not_spawn_fail` | — | — | — | — | — | — | — | |
| 1049 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1097 | `cluster_host_completes_claim_in_other_workspace` | — | — | — | — | — | — | — | |
| 1050 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1357 | `remote_success_with_result_md_is_done` | — | — | — | — | — | — | — | |
| 1051 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1471 | `complete_rejects_bad_machine_token` | — | — | — | — | — | — | — | |
| 1052 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1490 | `complete_rejects_wrong_lease_token` | — | — | — | — | — | — | — | |
| 1053 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1581 | `complete_rejects_another_hosts_claim` | — | — | — | — | — | — | — | |
| 1054 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1631 | `signal_after_done_success_does_not_revert` | — | — | — | — | — | — | — | |
| 1055 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_host_e2e.rs` | 1736 | `min_isolation_container_does_not_assign_process_only_host` | — | — | — | — | — | — | — | |
| 1056 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 576 | `unauthenticated_create_is_401` | — | — | — | — | — | — | — | |
| 1057 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 591 | `owner_create_and_get_list` | — | — | — | — | — | — | — | |
| 1058 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 739 | `worker_hosts_list_other_ws_is_403_and_leaks_no_inventory` | — | — | — | — | — | — | — | |
| 1059 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 1004 | `worker_hosts_mutate_other_ws_is_403_and_does_not_mint_or_revoke` | — | — | — | — | — | — | — | |
| 1060 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 1274 | `this_machine_origin_is_true_only_for_real_loopback` | — | — | — | — | — | — | — | |
| 1061 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 1449 | `host_create_name_empty_or_huge_is_400_and_inserts_no_row` | — | — | — | — | — | — | — | |
| 1062 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 1832 | `cancel_unused_pending_http_discards_never_paired_hidden_not_assignable` | — | — | — | — | — | — | — | |
| 1063 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 2297 | `done_keeps_unused_pending_host_after_create_without_delete` | — | — | — | — | — | — | — | |
| 1064 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 2469 | `install_hint_worker_public_base_not_api_public_base` | — | — | — | — | — | — | — | |
| 1065 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 2681 | `pair_ignores_host_header_for_control_plane_url` | — | — | — | — | — | — | — | |
| 1066 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 2976 | `pair_token_plaintext_only_on_create_not_later_get_or_list` | — | — | — | — | — | — | — | |
| 1067 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 3071 | `machine_token_plaintext_only_on_pair_or_bootstrap_not_later_get_heartbeat_or_lease` | — | — | — | — | — | — | — | |
| 1068 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 3309 | `owner_can_rename_host` | — | — | — | — | — | — | — | |
| 1069 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 3506 | `host_foreign_id_get_patch_delete_is_404_and_omits_workspace` | — | — | — | — | — | — | — | |
| 1070 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 3718 | `patch_labels_cluster_must_not_make_paired_host_visible_to_other_workspace` | — | — | — | — | — | — | — | |
| 1071 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4163 | `rename_host_patch_persists_rejects_empty_too_long` | — | — | — | — | — | — | — | |
| 1072 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4388 | `list_has_no_synthetic_control_plane` | — | — | — | — | — | — | — | |
| 1073 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4437 | `control_plane_does_not_pair_or_appear_assignable` | — | — | — | — | — | — | — | |
| 1074 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4669 | `isolation_defaults_native_and_patch_sandboxed` | — | — | — | — | — | — | — | |
| 1075 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4775 | `post_isolation_sandboxed_is_400_and_does_not_insert` | — | — | — | — | — | — | — | |
| 1076 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4812 | `post_kind_cloud_and_aws_http_does_not_create_assignable_host` | — | — | — | — | — | — | — | |
| 1077 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 4992 | `post_kind_cloud_patch_isolation_rejected_native_stays_assignable` | — | — | — | — | — | — | — | |
| 1078 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 5222 | `pair_token_expires_in_one_hour_and_cannot_be_reused` | — | — | — | — | — | — | — | |
| 1079 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 5394 | `pair_once_second_pair_rejected` | — | — | — | — | — | — | — | |
| 1080 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 5477 | `pair_empty_hostname_or_platform_is_400_and_does_not_consume` | — | — | — | — | — | — | — | |
| 1081 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 5853 | `pair_cookie_jwt_and_flm_cannot_complete_pair` | — | — | — | — | — | — | — | |
| 1082 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 6167 | `re_pair_same_hostname_reclaims_row` | — | — | — | — | — | — | — | |
| 1083 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 6270 | `heartbeat_and_lease_empty` | — | — | — | — | — | — | — | |
| 1084 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 6331 | `lease_returns_enqueued_spawn_session` | — | — | — | — | — | — | — | |
| 1085 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 6452 | `updating_host_lease_returns_claim_null` | — | — | — | — | — | — | — | |
| 1086 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 6957 | `expired_lease_returns_to_ready_next_me_lease_claims_again` | — | — | — | — | — | — | — | |
| 1087 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 7204 | `heartbeat_without_software_leaves_public_software_null` | — | — | — | — | — | — | — | |
| 1088 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 7283 | `admin_token_machine_cannot_authenticate_heartbeat_or_lease` | — | — | — | — | — | — | — | |
| 1089 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 7581 | `cookie_jwt_machine_cannot_authenticate_heartbeat_or_lease` | — | — | — | — | — | — | — | |
| 1090 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 7874 | `pair_token_machine_cannot_authenticate_heartbeat_or_lease` | — | — | — | — | — | — | — | |
| 1091 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 8409 | `heartbeat_agents_omit_tokens_from_persist_and_reads` | — | — | — | — | — | — | — | |
| 1092 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 8781 | `advertised_host_false_lease_null_even_if_advertised_container_true` | — | — | — | — | — | — | — | |
| 1093 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 9257 | `paused_host_lease_returns_claim_null` | — | — | — | — | — | — | — | |
| 1094 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 9660 | `paused_completes_claimed_lease` | — | — | — | — | — | — | — | |
| 1095 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10141 | `allow_host_false_lease_returns_claim_null` | — | — | — | — | — | — | — | |
| 1096 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10510 | `at_job_cap_lease_null_does_not_oversubscribe` | — | — | — | — | — | — | — | |
| 1097 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10830 | `lease_null_when_both_effective_modes_false` | — | — | — | — | — | — | — | |
| 1098 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10871 | `lease_null_when_only_container_is_advertised` | — | — | — | — | — | — | — | |
| 1099 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10913 | `lease_only_returns_claim_for_this_host` | — | — | — | — | — | — | — | |
| 1100 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 10952 | `complete_rejects_other_host_same_lease_token` | — | — | — | — | — | — | — | |
| 1101 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11021 | `lease_complete_cookie_jwt_cannot_finish_held_claim` | — | — | — | — | — | — | — | |
| 1102 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11224 | `lease_complete_garbage_token_keeps_claim` | — | — | — | — | — | — | — | |
| 1103 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11434 | `install_worker_sh_starts_with_shebang` | — | — | — | — | — | — | — | |
| 1104 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11446 | `install_linux_x86_64_script_ok` | — | — | — | — | — | — | — | |
| 1105 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11458 | `install_path_traversal_rejected` | — | — | — | — | — | — | — | |
| 1106 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11473 | `unset_worker_dist_scripts_serve_and_bin_get_404_with_build_hint` | — | — | — | — | — | — | — | |
| 1107 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11524 | `install_worker_bin_from_dist_dir` | — | — | — | — | — | — | — | |
| 1108 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11551 | `install_worker_bin_head_reports_length_without_body` | — | — | — | — | — | — | — | |
| 1109 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11582 | `install_worker_bin_dotdot_rejected` | — | — | — | — | — | — | — | |
| 1110 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11597 | `install_bin_allowlist_rejects_control_plane_and_unknown_names` | — | — | — | — | — | — | — | |
| 1111 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11806 | `install_worker_manifest_404_when_unstaged` | — | — | — | — | — | — | — | |
| 1112 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11821 | `install_worker_manifest_200_when_staged` | — | — | — | — | — | — | — | |
| 1113 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11847 | `heartbeat_includes_desired_when_staged` | — | — | — | — | — | — | — | |
| 1114 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11890 | `public_host_software_status_current_behind_unknown` | — | — | — | — | — | — | — | |
| 1115 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 11968 | `member_can_list_but_cannot_mutate` | — | — | — | — | — | — | — | |
| 1116 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12104 | `admin_cannot_mutate_worker_hosts` | — | — | — | — | — | — | — | |
| 1117 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12405 | `new_pair_token_only_while_pending_and_old_token_dies` | — | — | — | — | — | — | — | |
| 1118 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12501 | `request_update_marks_host` | — | — | — | — | — | — | — | |
| 1119 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12667 | `bootstrap_conflict_without_workspace` | — | — | — | — | — | — | — | |
| 1120 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12703 | `bootstrap_rejects_bad_secret` | — | — | — | — | — | — | — | |
| 1121 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12740 | `bootstrap_secret_unset_disables_route` | — | — | — | — | — | — | — | |
| 1122 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 12993 | `bootstrap_pairs_and_rotates_same_machine` | — | — | — | — | — | — | — | |
| 1123 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 13120 | `cluster_host_foreign_workspace_cannot_pause_or_revoke` | — | — | — | — | — | — | — | |
| 1124 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 13448 | `bootstrap_unknown_workspace_id_is_404_and_inserts_no_host` | — | — | — | — | — | — | — | |
| 1125 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 13770 | `install_ack_mismatch_keeps_queued_request` | — | — | — | — | — | — | — | |
| 1126 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 14228 | `install_model_rejects_shell_metachar_empty_and_non_ollama` | — | — | — | — | — | — | — | |
| 1127 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 14632 | `runtime_settings_rejects_shell_metachar_and_heartbeat_omits_secrets` | — | — | — | — | — | — | — | |
| 1128 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 15022 | `local_models_requires_auth_including_local_model_tags` | — | — | — | — | — | — | — | |
| 1129 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 15273 | `local_model_tags_rejects_unsafe_slug_other_ws_is_403` | — | — | — | — | — | — | — | |
| 1130 | `rust-integration` | `default` | `crates/fleet-server/tests/worker_hosts_http.rs` | 15479 | `max_job_timeout_below_60_is_400_and_stored_value_unchanged` | — | — | — | — | — | — | — | |

## fleet-ui

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1131 | `unit-ui-host` | `default` | `crates/fleet-ui/src/clipboard.rs` | 161 | `http_lan_fallback_is_textarea_before_clipboard_api` | — | — | — | — | — | — | — | |
| 1132 | `unit-ui-host` | `default` | `crates/fleet-ui/src/clipboard.rs` | 175 | `missing_clipboard_api_does_not_panic` | — | — | — | — | — | — | — | |
| 1133 | `unit-ui-host` | `default` | `crates/fleet-ui/src/components/sidebar.rs` | 764 | `user_menu_overlay_dismisses_and_opener_stays_clickable` | — | — | — | — | — | — | — | |
| 1134 | `unit-ui-host` | `default` | `crates/fleet-ui/src/components/toast.rs` | 268 | `merge_replaces_previous_success` | — | — | — | — | — | — | — | |
| 1135 | `unit-ui-host` | `default` | `crates/fleet-ui/src/components/toast.rs` | 276 | `merge_keeps_error_when_success_arrives` | — | — | — | — | — | — | — | |
| 1136 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/cases/page.rs` | 505 | `detail_status_and_stage_are_separate_pills` | — | — | — | — | — | — | — | |
| 1137 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/chat/api.rs` | 653 | `parse_stream_line_reads_status_action_error_and_done` | — | — | — | — | — | — | — | |
| 1138 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/fleet/api.rs` | 324 | `start_2_task_fixture_body_is_chain_not_diamond` | — | — | — | — | — | — | — | |
| 1139 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/fleet/api.rs` | 341 | `start_fixture_posts_kind_chain_never_diamond` | — | — | — | — | — | — | — | |
| 1140 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/fleet/page.rs` | 720 | `start_2_task_button_posts_chain_fixture_and_does_not_tick` | — | — | — | — | — | — | — | |
| 1141 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/fleet/page.rs` | 766 | `start_2_task_button_uses_accent_foreground_not_hardcoded_white` | — | — | — | — | — | — | — | |
| 1142 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 176 | `cloud_page_is_four_coming_soon_cards` | — | — | — | — | — | — | — | |
| 1143 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 198 | `cloud_page_has_no_spawn_or_lend_controls` | — | — | — | — | — | — | — | |
| 1144 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 212 | `cloud_page_cards_are_not_assignable` | — | — | — | — | — | — | — | |
| 1145 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 238 | `workspace_id_does_not_enable_cloud_enqueue` | — | — | — | — | — | — | — | |
| 1146 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 255 | `enabled_spawn_control_makes_cloud_page_assignable` | — | — | — | — | — | — | — | |
| 1147 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 274 | `enabled_lend_control_makes_cloud_page_assignable` | — | — | — | — | — | — | — | |
| 1148 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 296 | `disabled_spawn_control_still_counts_as_spawn_surface` | — | — | — | — | — | — | — | |
| 1149 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 320 | `dropping_coming_soon_badge_is_assignable_risk` | — | — | — | — | — | — | — | |
| 1150 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 339 | `unknown_cloud_card_name_is_assignable_risk` | — | — | — | — | — | — | — | |
| 1151 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 426 | `cloud_page_source_stays_coming_soon_placeholders` | — | — | — | — | — | — | — | |
| 1152 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/cloud/page.rs` | 474 | `cloud_page_is_not_a_host_creating_picker` | — | — | — | — | — | — | — | |
| 1153 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 285 | `partners_page_is_coming_soon_with_trust_copy` | — | — | — | — | — | — | — | |
| 1154 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 326 | `partners_page_disclaimer_says_ack_is_not_lend` | — | — | — | — | — | — | — | |
| 1155 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 352 | `partners_page_ack_does_not_enable_lend_or_balance` | — | — | — | — | — | — | — | |
| 1156 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 377 | `partners_page_lend_and_buy_exist_but_stay_disabled` | — | — | — | — | — | — | — | |
| 1157 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 393 | `partners_page_workspace_id_does_not_change_surface` | — | — | — | — | — | — | — | |
| 1158 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 411 | `partners_page_enabled_lend_breaks_ack_is_not_lend` | — | — | — | — | — | — | — | |
| 1159 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 424 | `partners_page_showing_balance_breaks_ack_is_not_lend` | — | — | — | — | — | — | — | |
| 1160 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 442 | `partners_page_enabling_select_breaks_ack_is_not_lend` | — | — | — | — | — | — | — | |
| 1161 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 457 | `partners_page_dropping_coming_soon_badge` | — | — | — | — | — | — | — | |
| 1162 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/partners/page.rs` | 556 | `partners_page_source_cannot_enable_lend_or_show_balance` | — | — | — | — | — | — | — | |
| 1163 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/runners/redirects.rs` | 48 | `runners_and_workers_redirect_to_self_hosted_not_blank_accordion` | — | — | — | — | — | — | — | |
| 1164 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 289 | `session_item_deserializes_as_sandbox_row` | — | — | — | — | — | — | — | |
| 1165 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 318 | `workers_envelope_deserializes_same_rows_as_sessions` | — | — | — | — | — | — | — | |
| 1166 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 344 | `workers_sandbox_ids_match_session_ids_in_order` | — | — | — | — | — | — | — | |
| 1167 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 362 | `empty_sessions_is_empty_sandboxes` | — | — | — | — | — | — | — | |
| 1168 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 374 | `empty_backend_displays_as_stub` | — | — | — | — | — | — | — | |
| 1169 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 384 | `missing_optional_session_fields_default` | — | — | — | — | — | — | — | |
| 1170 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 405 | `missing_required_session_id_fails` | — | — | — | — | — | — | — | |
| 1171 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 415 | `chat_history_item_is_not_a_sandbox_row` | — | — | — | — | — | — | — | |
| 1172 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 431 | `last_heartbeat_at_does_not_break_parse` | — | — | — | — | — | — | — | |
| 1173 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 440 | `list_path_is_workers_not_sessions` | — | — | — | — | — | — | — | |
| 1174 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 451 | `kill_path_uses_session_id` | — | — | — | — | — | — | — | |
| 1175 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 463 | `card_model_exposes_session_fields` | — | — | — | — | — | — | — | |
| 1176 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 486 | `live_statuses_are_not_a_kill_button` | — | — | — | — | — | — | — | |
| 1177 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 508 | `exited_card_shows_reason_and_timestamp` | — | — | — | — | — | — | — | |
| 1178 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 521 | `short_id_and_format_ts_edges` | — | — | — | — | — | — | — | |
| 1179 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 535 | `status_pills_group_known_session_statuses` | — | — | — | — | — | — | — | |
| 1180 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 551 | `page_copy_names_sessions_as_sandboxes` | — | — | — | — | — | — | — | |
| 1181 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 560 | `remapping_sessions_without_array_fails_closed` | — | — | — | — | — | — | — | |
| 1182 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 570 | `page_rsx_binds_sandboxes_copy_and_session_cards` | — | — | — | — | — | — | — | |
| 1183 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sandboxes/shape.rs` | 596 | `page_rsx_has_no_destroy_or_kill_button` | — | — | — | — | — | — | — | |
| 1184 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 240 | `chat_history_paths_are_not_worker_live` | — | — | — | — | — | — | — | |
| 1185 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 252 | `row_click_opens_chat_thread_not_worker_live` | — | — | — | — | — | — | — | |
| 1186 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 263 | `list_api_is_conversations_not_fleet_sessions` | — | — | — | — | — | — | — | |
| 1187 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 278 | `chat_history_item_parses_worker_session_does_not` | — | — | — | — | — | — | — | |
| 1188 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 293 | `page_copy_is_chat_history_not_sandboxes` | — | — | — | — | — | — | — | |
| 1189 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 306 | `sessions_list_rsx_loads_conversations_and_opens_chat` | — | — | — | — | — | — | — | |
| 1190 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 342 | `session_detail_rsx_is_fleet_live_view` | — | — | — | — | — | — | — | |
| 1191 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 367 | `live_session_back_link_goes_to_chat_history_not_fleet` | — | — | — | — | — | — | — | |
| 1192 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 425 | `session_detail_client_is_fleet_not_chat_list` | — | — | — | — | — | — | — | |
| 1193 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 442 | `chat_list_client_hits_conversations` | — | — | — | — | — | — | — | |
| 1194 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 464 | `fleet_cards_open_worker_live_view` | — | — | — | — | — | — | — | |
| 1195 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 486 | `live_sessions_nav_is_chat_history_not_sandboxes` | — | — | — | — | — | — | — | |
| 1196 | `unit-ui-host` | `default` | `crates/fleet-ui/src/pages/sessions/shape.rs` | 510 | `dioxus_routes_keep_live_view_off_the_list` | — | — | — | — | — | — | — | |
| 1197 | `unit-ui-host` | `default` | `crates/fleet-ui/src/theme.rs` | 88 | `from_storage_maps_aliases` | — | — | — | — | — | — | — | |

## sandbox-lab

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1198 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_compose_image_pins.py` | 16 | `test_compose_declares_stable_image_and_keeps_build` | — | — | — | — | — | — | — | |
| 1199 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_compose_image_pins.py` | 29 | `test_install_script_builds_pinned_names` | — | — | — | — | — | — | — | |
| 1200 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_gitssh_live.py` | 49 | `test_workspace_agent_clones_local_gitssh` | — | — | — | — | — | — | — | |
| 1201 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_host_e2e.py` | 140 | `test_agent_provision_and_wsexec` | — | — | — | — | — | — | — | |
| 1202 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_host_e2e.py` | 392 | `test_hmac_rejects_wrong_key_missing_sig_truncated_mac_and_replay` | — | — | — | — | — | — | — | |
| 1203 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_host_services.py` | 29 | `test_ollama_or_skip` | — | — | — | — | — | — | — | |
| 1204 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_host_services.py` | 39 | `test_openai_compat_proxy_or_skip` | — | — | — | — | — | — | — | |
| 1205 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_host_services.py` | 48 | `test_host_postgres` | — | — | — | — | — | — | — | |
| 1206 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 45 | `test_crds_dir_is_a_kustomization` | — | — | — | — | — | — | — | |
| 1207 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 50 | `test_kind_overlay_references_directories_not_crd_file` | — | — | — | — | — | — | — | |
| 1208 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 57 | `test_k3d_overlay_builds` | — | — | — | — | — | — | — | |
| 1209 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 74 | `test_kind_overlay_builds` | — | — | — | — | — | — | — | |
| 1210 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 93 | `test_base_infra_builds` | — | — | — | — | — | — | — | |
| 1211 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 104 | `test_k3d_overlay_has_in_cluster_k8s_path` | — | — | — | — | — | — | — | |
| 1212 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_infra_kustomize.py` | 140 | `test_api_nodeport_netpol_allows_port_8080_without_from` | — | — | — | — | — | — | — | |
| 1213 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 15 | `test_script_exists_and_executable` | — | — | — | — | — | — | — | |
| 1214 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 19 | `test_bash_syntax` | — | — | — | — | — | — | — | |
| 1215 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 23 | `test_rejects_arguments` | — | — | — | — | — | — | — | |
| 1216 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 34 | `test_sources_repo_root_env` | — | — | — | — | — | — | — | |
| 1217 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 41 | `test_documents_profiles` | — | — | — | — | — | — | — | |
| 1218 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 54 | `test_guest_e2e_defaults_on_for_every_profile` | — | — | — | — | — | — | — | |
| 1219 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 63 | `test_header_lists_compatible_systems_and_tested` | — | — | — | — | — | — | — | |
| 1220 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 81 | `test_no_crate_version_pin_in_script` | — | — | — | — | — | — | — | |
| 1221 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 85 | `test_kind_rollout_after_load` | — | — | — | — | — | — | — | |
| 1222 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 97 | `test_docker_pull_retries_and_uses_local_cache` | — | — | — | — | — | — | — | |
| 1223 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 108 | `test_kubectl_kind_stage_under_repo_tmp` | — | — | — | — | — | — | — | |
| 1224 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 114 | `test_browser_e2e_default_full_only` | — | — | — | — | — | — | — | |
| 1225 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 132 | `test_playwright_installs_os_deps` | — | — | — | — | — | — | — | |
| 1226 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 162 | `test_full_suite_gates_browser_e2e` | — | — | — | — | — | — | — | |
| 1227 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 172 | `test_agents_md_points_at_script` | — | — | — | — | — | — | — | |
| 1228 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 196 | `test_human_testing_doc_and_recorder_exist` | — | — | — | — | — | — | — | |
| 1229 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 240 | `test_required_packages_are_not_skip_unavailable` | — | — | — | — | — | — | — | |
| 1230 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 259 | `test_lmstudio_proxy_is_fail_closed` | — | — | — | — | — | — | — | |
| 1231 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 264 | `test_harness_images_skip_only_on_src_hash` | — | — | — | — | — | — | — | |
| 1232 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 269 | `test_does_not_copy_host_grok_auth_into_lab` | — | — | — | — | — | — | — | |
| 1233 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 274 | `test_kubectl_kind_version_not_swallowed` | — | — | — | — | — | — | — | |
| 1234 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_contract.py` | 279 | `test_ollama_reuses_healthy_listener` | — | — | — | — | — | — | — | |
| 1235 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_dx.py` | 16 | `test_set_e_install_dx_or_exit_1` | — | — | — | — | — | — | — | |
| 1236 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_dx.py` | 35 | `test_bash_syntax` | — | — | — | — | — | — | — | |
| 1237 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_dx.py` | 43 | `test_one_command_no_cli_flags_profile_enable_live_in_env` | — | — | — | — | — | — | — | |
| 1238 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 33 | `test_pair_token_env_in_unix_and_windows` | — | — | — | — | — | — | — | |
| 1239 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 42 | `test_bootstrap_exists_and_executable` | — | — | — | — | — | — | — | |
| 1240 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 46 | `test_all_top10_scripts_present` | — | — | — | — | — | — | — | |
| 1241 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 51 | `test_posix_syntax` | — | — | — | — | — | — | — | |
| 1242 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 72 | `test_detect_only_prints_known_id` | — | — | — | — | — | — | — | |
| 1243 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 98 | `test_detect_armv7_musl_prints_id_not_die` | — | — | — | — | — | — | — | |
| 1244 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 129 | `test_piped_without_url_fails_closed` | — | — | — | — | — | — | — | |
| 1245 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 145 | `test_windows_arm64_fetches_libunwind_optional` | — | — | — | — | — | — | — | |
| 1246 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 156 | `test_ps1_ascii_only_for_windows_powershell_51` | — | — | — | — | — | — | — | |
| 1247 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 169 | `test_scripts_use_fetch_and_keep_dest_leak_fix` | — | — | — | — | — | — | — | |
| 1248 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 190 | `test_install_uses_worker_product_name_and_three_bins` | — | — | — | — | — | — | — | |
| 1249 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 220 | `test_install_prints_progress_not_silent_downloads` | — | — | — | — | — | — | — | |
| 1250 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 257 | `test_stage_write_manifest_for_existing_dir` | — | — | — | — | — | — | — | |
| 1251 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 342 | `test_stage_manifest_missing_platform_or_hash_not_current` | — | — | — | — | — | — | — | |
| 1252 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 458 | `test_install_skips_download_when_sha256_matches` | — | — | — | — | — | — | — | |
| 1253 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 484 | `test_windows_bootstrap_uses_os_architecture` | — | — | — | — | — | — | — | |
| 1254 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 500 | `test_windows_refuses_wrong_arch_and_does_not_hide_errors` | — | — | — | — | — | — | — | |
| 1255 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 526 | `test_reinstall_restarts_already_paired_host` | — | — | — | — | — | — | — | |
| 1256 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 564 | `test_every_platform_starts_daemon_after_pair` | — | — | — | — | — | — | — | |
| 1257 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 599 | `test_fetch_only_path_installs_freebsd_bins` | — | — | — | — | — | — | — | |
| 1258 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 755 | `test_linux_curl_path_prints_progress_and_names_bins` | — | — | — | — | — | — | — | |
| 1259 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 840 | `test_url_host_strips_userinfo_like_rust` | — | — | — | — | — | — | — | |
| 1260 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 867 | `test_loopback_missing_health_dies_before_mkdir_or_fetch` | — | — | — | — | — | — | — | |
| 1261 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_install_worker.py` | 935 | `test_missing_or_whitespace_pair_token_dies_before_bins` | — | — | — | — | — | — | — | |
| 1262 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 170 | `test_detection_is_not_host_http_health` | — | — | — | — | — | — | — | |
| 1263 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 179 | `test_kind_smoke_covers_in_cluster_k8s_path` | — | — | — | — | — | — | — | |
| 1264 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 204 | `test_control_plane_deployments_ready` | — | — | — | — | — | — | — | |
| 1265 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 218 | `test_crd_installed` | — | — | — | — | — | — | — | |
| 1266 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 223 | `test_host_api_health` | — | — | — | — | — | — | — | |
| 1267 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 234 | `test_host_ui` | — | — | — | — | — | — | — | |
| 1268 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 256 | `test_api_is_not_a_runner_and_host_owns_crs` | — | — | — | — | — | — | — | |
| 1269 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 281 | `test_api_image_has_driver_not_worker` | — | — | — | — | — | — | — | |
| 1270 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 301 | `test_tick_creates_and_cancel_deletes_fleetsandbox` | — | — | — | — | — | — | — | |
| 1271 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_kind_smoke.py` | 477 | `test_second_workspace_cluster_host_still_creates_fsb` | — | — | — | — | — | — | — | |
| 1272 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 11 | `test_profile_full_without_enable_kind_is_on` | — | — | — | — | — | — | — | |
| 1273 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 20 | `test_explicit_zero_beats_profile_full` | — | — | — | — | — | — | — | |
| 1274 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 25 | `test_skip_kind_forces_off` | — | — | — | — | — | — | — | |
| 1275 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 29 | `test_process_env_used_when_dotenv_omits_key` | — | — | — | — | — | — | — | |
| 1276 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 38 | `test_lab_profile_kind_off_ollama_on` | — | — | — | — | — | — | — | |
| 1277 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_lab_env.py` | 46 | `test_minimal_proxy_and_gitssh_off` | — | — | — | — | — | — | — | |
| 1278 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_publish_lab_job.py` | 36 | `test_image_is_pi_does_not_match_alpine` | — | — | — | — | — | — | — | |
| 1279 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_publish_lab_job.py` | 43 | `test_overlay_rewrites_lmstudio_job_to_ollama` | — | — | — | — | — | — | — | |
| 1280 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_publish_lab_job.py` | 67 | `test_job_harness_wins_over_env` | — | — | — | — | — | — | — | |
| 1281 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_publish_lab_job.py` | 73 | `test_force_harness` | — | — | — | — | — | — | — | |
| 1282 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 103 | `test_procedures_and_requirements_exist` | — | — | — | — | — | — | — | |
| 1283 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 120 | `test_install_scripts_take_pair_token` | — | — | — | — | — | — | — | |
| 1284 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 132 | `test_flag_defaults_on` | — | — | — | — | — | — | — | |
| 1285 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 143 | `test_flag_explicit_zero_wins` | — | — | — | — | — | — | — | |
| 1286 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 158 | `test_required_guests_are_not_live_skipped` | — | — | — | — | — | — | — | |
| 1287 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 168 | `test_live_methods_exist_for_every_platform` | — | — | — | — | — | — | — | |
| 1288 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 176 | `test_missing_required_guest_fails_not_warn` | — | — | — | — | — | — | — | |
| 1289 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 198 | `test_live_skipped_guest_skips_not_fail` | — | — | — | — | — | — | — | |
| 1290 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 209 | `test_live_skipped_guests_remain_in_catalog` | — | — | — | — | — | — | — | |
| 1291 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 259 | `test_explicit_zero_skips_live_setup` | — | — | — | — | — | — | — | |
| 1292 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 269 | `test_catalog_tests_always_run` | — | — | — | — | — | — | — | |
| 1293 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 274 | `test_require_guest_source_is_fail_closed` | — | — | — | — | — | — | — | |
| 1294 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 381 | `test_linux_x86_64` | — | — | — | — | — | — | — | |
| 1295 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 384 | `test_linux_x86_64_musl` | — | — | — | — | — | — | — | |
| 1296 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 387 | `test_linux_x86_64_bookworm` | — | — | — | — | — | — | — | |
| 1297 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 390 | `test_linux_aarch64` | — | — | — | — | — | — | — | |
| 1298 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 393 | `test_linux_aarch64_musl` | — | — | — | — | — | — | — | |
| 1299 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 396 | `test_linux_armv7` | — | — | — | — | — | — | — | |
| 1300 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 399 | `test_linux_armv7_musl` | — | — | — | — | — | — | — | |
| 1301 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 402 | `test_freebsd_amd64` | — | — | — | — | — | — | — | |
| 1302 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 405 | `test_darwin_x86_64` | — | — | — | — | — | — | — | |
| 1303 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 408 | `test_darwin_arm64` | — | — | — | — | — | — | — | |
| 1304 | `guest-e2e` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 411 | `test_windows_x86_64` | — | — | — | — | — | — | — | |
| 1305 | `guest-e2e` | `live-skipped` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_runner_guest_e2e.py` | 414 | `test_windows_arm64` | — | — | — | — | — | — | — | |
| 1306 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_tools_by_job.py` | 114 | `test_TOOLS_BY_JOB_named_tools_take_argv_only_not_shell_interpolated` | — | — | — | — | — | — | — | |
| 1307 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_tools_by_job.py` | 301 | `test_TOOLS_BY_JOB_bin_must_be_in_ALLOWED_BINS` | — | — | — | — | — | — | — | |
| 1308 | `lab` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/tests/test_wsexec_signed_error.py` | 185 | `test_wsexec_unknown_bin_path_escape_bad_hmac_is_signed_error_not_a_shell` | — | — | — | — | — | — | — | |

## sandbox-lab/workspace

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1309 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 89 | `test_a_unknown_tool_rejected` | — | — | — | — | — | — | — | |
| 1310 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 103 | `test_b_known_tool_ok_stdout_structure` | — | — | — | — | — | — | — | |
| 1311 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 121 | `test_b2_echo_tool` | — | — | — | — | — | — | — | |
| 1312 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 132 | `test_c_cwd_outside_jobs_rejected` | — | — | — | — | — | — | — | |
| 1313 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 148 | `test_c2_parent_escape_rejected` | — | — | — | — | — | — | — | |
| 1314 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 153 | `test_raw_exec_disabled` | — | — | — | — | — | — | — | |
| 1315 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 160 | `test_raw_exec_policy_matches_rust` | — | — | — | — | — | — | — | |
| 1316 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 167 | `test_raw_exec_default_off_from_env` | — | — | — | — | — | — | — | |
| 1317 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 174 | `test_raw_exec_enabled_when_allow_flag` | — | — | — | — | — | — | — | |
| 1318 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 187 | `test_handle_request_exec_rejected_by_default` | — | — | — | — | — | — | — | |
| 1319 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 203 | `test_path_under_jobs_accepts_job_tree` | — | — | — | — | — | — | — | |
| 1320 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 207 | `test_handle_request_tool_writes_result` | — | — | — | — | — | — | — | |
| 1321 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 227 | `test_handle_request_unknown_tool_fails_result` | — | — | — | — | — | — | — | |
| 1322 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 240 | `test_unsigned_request_rejected_before_provision` | — | — | — | — | — | — | — | |
| 1323 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 258 | `test_hmac_tool_does_not_read_tools_file` | — | — | — | — | — | — | — | |
| 1324 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 281 | `test_unsigned_tool_after_register_uses_memory_argv` | — | — | — | — | — | — | — | |
| 1325 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 302 | `test_unsigned_tool_rejects_argv_override` | — | — | — | — | — | — | — | |
| 1326 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 319 | `test_git_ref_rejects_shell_metachar` | — | — | — | — | — | — | — | |
| 1327 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 325 | `test_git_clone_missing_ref_fails_closed` | — | — | — | — | — | — | — | |
| 1328 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 345 | `test_mutated_tools_file_ignored_after_register` | — | — | — | — | — | — | — | |
| 1329 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 359 | `test_curl_and_bash_argv_rejected` | — | — | — | — | — | — | — | |
| 1330 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 365 | `test_ssh_repo_detection` | — | — | — | — | — | — | — | |
| 1331 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 372 | `test_write_json_default_mode_is_group_readable` | — | — | — | — | — | — | — | |
| 1332 | `lab-workspace` | `default` | `crates/fleet-sandbox-driver/sandbox-lab/workspace/tests/test_workspace_agent_protocol.py` | 378 | `test_git_ssh_env_sets_command_when_key_present` | — | — | — | — | — | — | — | |

## tests/browser

| # | Layer | Gate | File | Line | Test | L1 | L2 | L3 | L4 | L5 | L6 | L7 | Notes |
| ---: | --- | --- | --- | ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1333 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 690 | `test_01_landing_has_cta` | — | — | — | — | — | — | — | |
| 1334 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 697 | `test_02_unauthenticated_app_shell_redirects_to_login` | — | — | — | — | — | — | — | |
| 1335 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 704 | `test_03_signup_then_login_reaches_dashboard` | — | — | — | — | — | — | — | |
| 1336 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 722 | `test_04_sidebar_app_pages` | — | — | — | — | — | — | — | |
| 1337 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 853 | `test_05_workers_add_shows_pair_token` | — | — | — | — | — | — | — | |
| 1338 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 930 | `test_15_cancel_discards_pending_worker` | — | — | — | — | — | — | — | |
| 1339 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 951 | `test_16_control_plane_card` | — | — | — | — | — | — | — | |
| 1340 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 965 | `test_16b_fleet_header_says_paired_hosts` | — | — | — | — | — | — | — | |
| 1341 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 981 | `test_fleet_run_until_done_caps_24_ticks` | — | — | — | — | — | — | — | |
| 1342 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1139 | `test_17_apply_changes_when_dirty` | — | — | — | — | — | — | — | |
| 1343 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1159 | `test_06_api_health_from_browser` | — | — | — | — | — | — | — | |
| 1344 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1174 | `test_07_sidebar_has_fleet_and_integrations` | — | — | — | — | — | — | — | |
| 1345 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1209 | `test_08_workers_redirects_to_self_hosted` | — | — | — | — | — | — | — | |
| 1346 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1214 | `test_09_runners_redirects_to_self_hosted` | — | — | — | — | — | — | — | |
| 1347 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1221 | `test_10_nav_links_to_cloud_and_partners` | — | — | — | — | — | — | — | |
| 1348 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1300 | `test_11_cloud_page_coming_soon` | — | — | — | — | — | — | — | |
| 1349 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1320 | `test_12_partners_page_trust_disclaimer` | — | — | — | — | — | — | — | |
| 1350 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1357 | `test_13_no_remote_enqueue_from_cloud_or_partners` | — | — | — | — | — | — | — | |
| 1351 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1417 | `test_14o_partners_ack` | — | — | — | — | — | — | — | |
| 1352 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1486 | `test_14f_specs_live` | — | — | — | — | — | — | — | |
| 1353 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1565 | `test_14k_show_removed` | — | — | — | — | — | — | — | |
| 1354 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1639 | `test_14m_apply_toast` | — | — | — | — | — | — | — | |
| 1355 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1731 | `test_14h_copy_buttons_http_lan_fallback` | — | — | — | — | — | — | — | |
| 1356 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 1995 | `test_14a_rename_card` | — | — | — | — | — | — | — | |
| 1357 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2060 | `test_14j_last_seen` | — | — | — | — | — | — | — | |
| 1358 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2117 | `test_14g_software_line` | — | — | — | — | — | — | — | |
| 1359 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2171 | `test_14c_remove_confirm` | — | — | — | — | — | — | — | |
| 1360 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2223 | `test_14b_pending_apply_outdated` | — | — | — | — | — | — | — | |
| 1361 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2366 | `test_14n_widgets_lock` | — | — | — | — | — | — | — | |
| 1362 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2461 | `test_14l_plane_sandboxed` | — | — | — | — | — | — | — | |
| 1363 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2477 | `test_14d_update_now` | — | — | — | — | — | — | — | |
| 1364 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2557 | `test_14i_loopback_warning` | — | — | — | — | — | — | — | |
| 1365 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2656 | `test_14e_poll_list` | — | — | — | — | — | — | — | |
| 1366 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2728 | `test_sandboxes_page_matches_sessions` | — | — | — | — | — | — | — | |
| 1367 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 2965 | `test_sandboxes_page_no_destroy_button` | — | — | — | — | — | — | — | |
| 1368 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 3158 | `test_sessions_page_lists_chats_not_workers` | — | — | — | — | — | — | — | |
| 1369 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 3320 | `test_chat_send_empty_model_blocked` | — | — | — | — | — | — | — | |
| 1370 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 3637 | `test_chat_openai_coming_soon_snaps_back` | — | — | — | — | — | — | — | |
| 1371 | `browser-e2e` | `default` | `tests/browser/test_app_e2e.py` | 4126 | `test_18_logout_returns_to_login` | — | — | — | — | — | — | — | |
| 1372 | `browser-e2e` | `default` | `tests/browser/test_cases_e2e.py` | 256 | `test_unauthenticated_cases_redirects_to_login` | — | — | — | — | — | — | — | |
| 1373 | `browser-e2e` | `default` | `tests/browser/test_cases_e2e.py` | 271 | `test_create_observe_cancel` | — | — | — | — | — | — | — | |
| 1374 | `browser-e2e` | `default` | `tests/browser/test_cases_e2e.py` | 377 | `test_cases_decide_complete` | — | — | — | — | — | — | — | |
| 1375 | `browser-e2e` | `default` | `tests/browser/test_catalog_apply_toast.py` | 167 | `test_apply_toast_and_pending_apply_until_ack` | — | — | — | — | — | — | — | |
| 1376 | `browser-e2e` | `default` | `tests/browser/test_catalog_cloud.py` | 208 | `test_cloud_page_coming_soon` | — | — | — | — | — | — | — | |
| 1377 | `browser-e2e` | `default` | `tests/browser/test_catalog_cloud.py` | 295 | `test_cloud_four_provider_cards_picker_does_not_create_hosts` | — | — | — | — | — | — | — | |
| 1378 | `browser-e2e` | `default` | `tests/browser/test_catalog_copy_buttons.py` | 274 | `test_copy_unix_windows_token_incl_http_lan_fallback` | — | — | — | — | — | — | — | |
| 1379 | `browser-e2e` | `default` | `tests/browser/test_catalog_last_seen.py` | 117 | `test_last_seen_never_then_just_now` | — | — | — | — | — | — | — | |
| 1380 | `browser-e2e` | `default` | `tests/browser/test_catalog_loopback_warning.py` | 223 | `test_warning_when_command_is_loopback` | — | — | — | — | — | — | — | |
| 1381 | `browser-e2e` | `default` | `tests/browser/test_catalog_loopback_warning.py` | 252 | `test_no_warning_when_command_is_public` | — | — | — | — | — | — | — | |
| 1382 | `browser-e2e` | `default` | `tests/browser/test_catalog_pair_done.py` | 170 | `test_done_hides_panel_keeps_pending_after_reload` | — | — | — | — | — | — | — | |
| 1383 | `browser-e2e` | `default` | `tests/browser/test_catalog_partners.py` | 163 | `test_ack_persists_and_does_not_enable_lend` | — | — | — | — | — | — | — | |
| 1384 | `browser-e2e` | `default` | `tests/browser/test_catalog_pills.py` | 153 | `test_pending_apply_until_ack_and_outdated` | — | — | — | — | — | — | — | |
| 1385 | `browser-e2e` | `default` | `tests/browser/test_catalog_plane_sandboxed.py` | 140 | `test_toggle_sandboxed_on_the_plane_card` | — | — | — | — | — | — | — | |
| 1386 | `browser-e2e` | `default` | `tests/browser/test_catalog_poll.py` | 163 | `test_card_updates_after_pair_heartbeat` | — | — | — | — | — | — | — | |
| 1387 | `browser-e2e` | `default` | `tests/browser/test_catalog_remove.py` | 162 | `test_remove_worker_confirm_removes_cancel_aborts` | — | — | — | — | — | — | — | |
| 1388 | `browser-e2e` | `default` | `tests/browser/test_catalog_rename.py` | 94 | `test_rename_card_enter_saves_escape_cancels` | — | — | — | — | — | — | — | |
| 1389 | `browser-e2e` | `default` | `tests/browser/test_catalog_show_removed.py` | 148 | `test_show_removed_after_revoke_of_a_paired_box` | — | — | — | — | — | — | — | |
| 1390 | `browser-e2e` | `default` | `tests/browser/test_catalog_software.py` | 139 | `test_control_plane_card_has_no_software_line` | — | — | — | — | — | — | — | |
| 1391 | `browser-e2e` | `default` | `tests/browser/test_catalog_software.py` | 144 | `test_paired_card_shows_bake_id_not_bin_hash` | — | — | — | — | — | — | — | |
| 1392 | `browser-e2e` | `default` | `tests/browser/test_catalog_software.py` | 183 | `test_pending_card_has_no_software_line` | — | — | — | — | — | — | — | |
| 1393 | `browser-e2e` | `default` | `tests/browser/test_catalog_specs.py` | 110 | `test_specs_on_a_live_card` | — | — | — | — | — | — | — | |
| 1394 | `browser-e2e` | `default` | `tests/browser/test_catalog_update_now.py` | 220 | `test_update_now_when_behind_becomes_update_requested` | — | — | — | — | — | — | — | |
| 1395 | `browser-e2e` | `default` | `tests/browser/test_catalog_widgets_lock.py` | 167 | `test_widgets_lock_while_pending_until_ack` | — | — | — | — | — | — | — | |
| 1396 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 151 | `test_forbidden_substitute_phrases_absent` | — | — | — | — | — | — | — | |
| 1397 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 171 | `test_only_install_script_installs_dx` | — | — | — | — | — | — | — | |
| 1398 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 195 | `test_bootstrap_does_not_install_or_warn_dx` | — | — | — | — | — | — | — | |
| 1399 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 203 | `test_every_test_requires_tools_and_host_dx_build` | — | — | — | — | — | — | — | |
| 1400 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 214 | `test_fleet_dev_does_not_skip_ui_without_dx` | — | — | — | — | — | — | — | |
| 1401 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 223 | `test_fleet_dev_remaps_when_kind_owns_ports` | — | — | — | — | — | — | — | |
| 1402 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 239 | `test_identity_has_no_allow_continue` | — | — | — | — | — | — | — | |
| 1403 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 250 | `test_orch_worker_url_is_config_not_live_env` | — | — | — | — | — | — | — | |
| 1404 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 261 | `test_health_sha_is_compile_time_only` | — | — | — | — | — | — | — | |
| 1405 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 274 | `test_identity_cannot_be_satisfied_by_kubectl_env_or_foreign_allow` | — | — | — | — | — | — | — | |
| 1406 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 355 | `test_live_identity_fails_on_mismatch_despite_kubectl_env_and_foreign_allow` | — | — | — | — | — | — | — | |
| 1407 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 521 | `test_dockerfile_bakes_sha_on_consuming_compile` | — | — | — | — | — | — | — | |
| 1408 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 561 | `test_compose_requires_sha_build_arg` | — | — | — | — | — | — | — | |
| 1409 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 570 | `test_ui_image_embeds_sha_file_and_label` | — | — | — | — | — | — | — | |
| 1410 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 580 | `test_operator_image_labeled` | — | — | — | — | — | — | — | |
| 1411 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 585 | `test_install_installs_dx_and_rebuilds_stale_local_tags` | — | — | — | — | — | — | — | |
| 1412 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 598 | `test_local_images_skip_rebuild_only_when_label_matches_tree_id` | — | — | — | — | — | — | — | |
| 1413 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 825 | `test_tree_id_lock_requires_rebuild_after_changes` | — | — | — | — | — | — | — | |
| 1414 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 894 | `test_agents_md_forbids_substitutes` | — | — | — | — | — | — | — | |
| 1415 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 905 | `test_agents_md_test_writer_is_independent_not_full_rerun` | — | — | — | — | — | — | — | |
| 1416 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 917 | `test_tools_gate_fails_when_dx_absent` | — | — | — | — | — | — | — | |
| 1417 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 935 | `test_human_and_every_do_not_set_skip_ui` | — | — | — | — | — | — | — | |
| 1418 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 941 | `test_full_suite_rejects_foreign_allow` | — | — | — | — | — | — | — | |
| 1419 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 948 | `test_bootstrap_postgres_wait_is_fail_closed` | — | — | — | — | — | — | — | |
| 1420 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 953 | `test_install_does_not_skip_unavailable_required_rpms` | — | — | — | — | — | — | — | |
| 1421 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 962 | `test_host_dx_build_is_portable` | — | — | — | — | — | — | — | |
| 1422 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 967 | `test_ui_dockerfile_sha_after_dx_install` | — | — | — | — | — | — | — | |
| 1423 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 983 | `test_operator_compiles_before_sha_label` | — | — | — | — | — | — | — | |
| 1424 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 995 | `test_merge_to_main_requires_green_tests` | — | — | — | — | — | — | — | |
| 1425 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 1015 | `test_gitlab_ci_stays_workflow_when_never` | — | — | — | — | — | — | — | |
| 1426 | `source-contract` | `default` | `tests/browser/test_fail_closed_contract.py` | 1066 | `test_lab_env_honors_profile_full_defaults` | — | — | — | — | — | — | — | |
| 1427 | `source-contract` | `default` | `tests/browser/test_gitlab_ci_contract.py` | 69 | `test_gitlab_ci_yml_exists` | — | — | — | — | — | — | — | |
| 1428 | `source-contract` | `default` | `tests/browser/test_gitlab_ci_contract.py` | 75 | `test_only_workflow_rules_when_never` | — | — | — | — | — | — | — | |
| 1429 | `source-contract` | `default` | `tests/browser/test_gitlab_ci_contract.py` | 87 | `test_no_job_keys_or_auto_devops` | — | — | — | — | — | — | — | |
| 1430 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 402 | `test_artifacts_exist` | — | — | — | — | — | — | — | |
| 1431 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 418 | `test_policy_docs_require_same_change` | — | — | — | — | — | — | — | |
| 1432 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 461 | `test_film_and_human_test_are_scoped_to_the_merge` | — | — | — | — | — | — | — | |
| 1433 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 501 | `test_this_mr_step_requires_step_and_scene` | — | — | — | — | — | — | — | |
| 1434 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 515 | `test_demo_videos_are_not_git_added` | — | — | — | — | — | — | — | |
| 1435 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 549 | `test_record_invokes_record_demo` | — | — | — | — | — | — | — | |
| 1436 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 558 | `test_every_test_runs_this_contract` | — | — | — | — | — | — | — | |
| 1437 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 567 | `test_skip_maps_cannot_hide_a_new_surface` | — | — | — | — | — | — | — | |
| 1438 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 628 | `test_user_visible_pages_have_scene_step_and_docs` | — | — | — | — | — | — | — | |
| 1439 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 688 | `test_declared_page_mods_are_inventoried` | — | — | — | — | — | — | — | |
| 1440 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 701 | `test_user_visible_routes_have_scene_and_step` | — | — | — | — | — | — | — | |
| 1441 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 758 | `test_runner_nav_children_have_scenes` | — | — | — | — | — | — | — | |
| 1442 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 813 | `test_required_scenes_are_implemented` | — | — | — | — | — | — | — | |
| 1443 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 860 | `test_scenes_dict_maps_to_matching_demo_method` | — | — | — | — | — | — | — | |
| 1444 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 878 | `test_sidebar_nav_routes_have_scenes` | — | — | — | — | — | — | — | |
| 1445 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 936 | `test_page_inventory_picks_up_a_new_folder` | — | — | — | — | — | — | — | |
| 1446 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 958 | `test_pause_mentions_counts_titles_not_bodies` | — | — | — | — | — | — | — | |
| 1447 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 979 | `test_new_surface_needs_scene_step_docs_and_core` | — | — | — | — | — | — | — | |
| 1448 | `source-contract` | `default` | `tests/browser/test_human_surface_contract.py` | 1075 | `test_fn_is_stub_rejects_pass_only_scene` | — | — | — | — | — | — | — | |
| 1449 | `browser-e2e` | `default` | `tests/browser/test_inbox_e2e.py` | 189 | `test_unauthenticated_inbox_redirects_to_login` | — | — | — | — | — | — | — | |
| 1450 | `browser-e2e` | `default` | `tests/browser/test_inbox_e2e.py` | 204 | `test_list_monitor_finish` | — | — | — | — | — | — | — | |
| 1451 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 169 | `C0 list 200` | — | — | — | — | — | — | — | |
| 1452 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 181 | `C1 github connected or error (token)` | — | — | — | — | — | — | — | |
| 1453 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 188 | `C1 slack connected` | — | — | — | — | — | — | — | |
| 1454 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 189 | `C1 notion connected` | — | — | — | — | — | — | — | |
| 1455 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 190 | `C2 slack oauth_configured` | — | — | — | — | — | — | — | |
| 1456 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 191 | `C2 notion oauth_configured` | — | — | — | — | — | — | — | |
| 1457 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 197 | `C3 github account_label` | — | — | — | — | — | — | — | |
| 1458 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 197 | `C3 notion account_label` | — | — | — | — | — | — | — | |
| 1459 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 197 | `C3 slack account_label` | — | — | — | — | — | — | — | |
| 1460 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 198 | `C4 jira not connected` | — | — | — | — | — | — | — | |
| 1461 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 211 | `I1 monitor 200` | — | — | — | — | — | — | — | |
| 1462 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 214 | `I1 monitor providers` | — | — | — | — | — | — | — | |
| 1463 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 220 | `I2 poll 200` | — | — | — | — | — | — | — | |
| 1464 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 224 | `I2 poll not_connected` | — | — | — | — | — | — | — | |
| 1465 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 230 | `I3 inbox 200` | — | — | — | — | — | — | — | |
| 1466 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 233 | `I3 inbox list` | — | — | — | — | — | — | — | |
| 1467 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 237 | `H1 status 200` | — | — | — | — | — | — | — | |
| 1468 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 240 | `H1 hint mentions runner or OpenAI` | — | — | — | — | — | — | — | |
| 1469 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 254 | `H2 grok without runner is 400` | — | — | — | — | — | — | — | |
| 1470 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 293 | `H3 grok+runner HTTP` | — | — | — | — | — | — | — | |
| 1471 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 300 | `H3 runner CLI has no integration actions` | — | — | — | — | — | — | — | |
| 1472 | `live-integrations` | `live-flag` | `tests/browser/test_integrations_live.py` | 306 | `H3 runner returned text` | — | — | — | — | — | — | — | |
| 1473 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 134 | `test_recorder_and_docs_exist` | — | — | — | — | — | — | — | |
| 1474 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 140 | `test_list_exits_zero_and_prints_every_scene` | — | — | — | — | — | — | — | |
| 1475 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 157 | `test_scenes_dict_keys_match_list` | — | — | — | — | — | — | — | |
| 1476 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 174 | `test_human_testing_doc_mentions_each_scene` | — | — | — | — | — | — | — | |
| 1477 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 184 | `test_human_test_sh_pause_steps` | — | — | — | — | — | — | — | |
| 1478 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 194 | `test_require_stack_uses_this_tree_identity` | — | — | — | — | — | — | — | |
| 1479 | `source-contract` | `default` | `tests/browser/test_record_demo_contract.py` | 214 | `test_clicks_are_crossed_with_server_log` | — | — | — | — | — | — | — | |
| 1480 | `browser-e2e` | `default` | `tests/browser/test_settings_members_e2e.py` | 334 | `test_unauthenticated_settings_redirects_to_login` | — | — | — | — | — | — | — | |
| 1481 | `browser-e2e` | `default` | `tests/browser/test_settings_members_e2e.py` | 349 | `test_owner_invite_existing_role_remove_and_pending_revoke` | — | — | — | — | — | — | — | |
| 1482 | `browser-e2e` | `default` | `tests/browser/test_settings_members_e2e.py` | 490 | `test_pending_invite_joins_on_signup` | — | — | — | — | — | — | — | |
| 1483 | `browser-e2e` | `default` | `tests/browser/test_settings_members_e2e.py` | 564 | `test_member_cannot_invite` | — | — | — | — | — | — | — | |
| 1484 | `browser-e2e` | `default` | `tests/browser/test_shell_theme_e2e.py` | 120 | `test_appearance_toggle_and_escape_closes_overlay` | — | — | — | — | — | — | — | |
| 1485 | `browser-e2e` | `default` | `tests/browser/test_shell_theme_e2e.py` | 162 | `test_fleet_start_button_readable_in_dark` | — | — | — | — | — | — | — | |
| 1486 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 19 | `test_every_scene_has_a_map_rule` | — | — | — | — | — | — | — | |
| 1487 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 32 | `test_map_paths_exist_or_are_prefixes` | — | — | — | — | — | — | — | |
| 1488 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 42 | `test_skip_docs_and_tests` | — | — | — | — | — | — | — | |
| 1489 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 48 | `test_indirect_db_chat_repo_pulls_chat_and_sessions` | — | — | — | — | — | — | — | |
| 1490 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 60 | `test_parse_roundtrip` | — | — | — | — | — | — | — | |
| 1491 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 76 | `test_scope_assignment_is_literal_in_record_demo` | — | — | — | — | — | — | — | |
| 1492 | `source-contract` | `default` | `tests/browser/test_surface_scope.py` | 82 | `test_write_check_agree_on_this_tree` | — | — | — | — | — | — | — | |
| 1493 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 24 | `test_image_ships_driver_and_mock_agent_not_fleet_worker` | — | — | — | — | — | — | — | |
| 1494 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 47 | `test_orchestrator_is_a_library_not_a_second_control_plane` | — | — | — | — | — | — | — | |
| 1495 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 69 | `test_install_and_stage_ship_host_plus_driver_only` | — | — | — | — | — | — | — | |
| 1496 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 83 | `test_control_plane_yaml_must_not_inject_guest_harness` | — | — | — | — | — | — | — | |
| 1497 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 91 | `test_docker_and_local_sandboxes_must_not_use_in_process_agent_job` | — | — | — | — | — | — | — | |
| 1498 | `source-contract` | `default` | `tests/browser/test_unify_sku_contract.py` | 112 | `test_plane_docker_default_network_must_let_guests_reach_the_plane` | — | — | — | — | — | — | — | |
| 1499 | `browser-e2e` | `default` | `tests/browser/test_unify_usability.py` | 570 | `test_menu_drive_with_backend_states` | — | — | — | — | — | — | — | |

---

End of matrix. 1499 rows. Criteria columns are unevaluated.

