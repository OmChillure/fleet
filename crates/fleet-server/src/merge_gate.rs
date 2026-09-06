//! Lock: `just test-integration` and `scripts/run-every-test.sh`
//! orch-integration keep the live orch `--test` list; the justfile
//! `test` / `test-integration` recipes have `cargo test` and no `|| true`.

const LIVE_ORCH_INTEGRATION_BINS: &[&str] = &[
    "store_integration",
    "tick_integration",
    "api_integration",
    "e2e_matrix",
    "host_pool_spawn",
    "host_update_tick",
    "assignable_runner",
    "git_config_patch",
    "host_picker_product",
];

fn justfile_recipe<'a>(justfile: &'a str, header: &str) -> &'a str {
    justfile
        .split(header)
        .nth(1)
        .and_then(|s| s.split("\n# ").next())
        .unwrap_or(justfile)
}

fn assert_live_orch_bins(label: &str, src: &str) {
    assert!(
        !src.contains("process_sandbox") && !src.contains("plane_runner_tick"),
        "{label} must not name deleted orch test binaries:\n{src}"
    );
    for needle in LIVE_ORCH_INTEGRATION_BINS {
        assert!(
            src.contains(needle),
            "{label} must run {needle}:\n{src}"
        );
    }
}

#[test]
fn merge_to_main_requires_green_tests() {
    let justfile = include_str!("../../../justfile");
    let integration = justfile_recipe(justfile, "\ntest-integration:");
    assert_live_orch_bins("just test-integration", integration);
}

#[test]
fn run_every_test_sh_orch_integration_matches_live_bins() {
    let every = include_str!("../../../scripts/run-every-test.sh");
    let orch = every
        .split("run_step orch-integration")
        .nth(1)
        .and_then(|s| s.split("run_step ").next())
        .unwrap_or(every);
    assert_live_orch_bins("scripts/run-every-test.sh orch-integration", orch);
}

#[test]
fn justfile_test_and_test_integration_have_no_or_true_on_cargo() {
    let justfile = include_str!("../../../justfile");
    let unit = justfile_recipe(justfile, "\ntest:\n");
    let integration = justfile_recipe(justfile, "\ntest-integration:");
    for (label, src) in [("test", unit), ("test-integration", integration)] {
        assert!(
            src.contains("cargo test"),
            "just {label} must contain cargo test:\n{src}"
        );
        assert!(
            !src.contains("|| true"),
            "just {label} must not wrap cargo in || true:\n{src}"
        );
    }
}

#[test]
fn justfile_test_integration_runs_auth_server_and_db_package_tests() {
    let justfile = include_str!("../../../justfile");
    let integration = justfile_recipe(justfile, "\ntest-integration:");
    for needle in [
        "cargo test -p fleet-auth --locked --tests",
        "cargo test -p fleet-server --locked --tests",
        "cargo test -p fleet-db --locked --tests",
    ] {
        assert!(
            integration.contains(needle),
            "just test-integration must run {needle}:\n{integration}"
        );
    }
}
