//! Lock: all tests must pass before a merge to main, and every test
//! must be relevant.
//!
//! This GitLab has no runner (`.gitlab-ci.yml` is `when: never`). The
//! merge gate is the text in AGENTS.md / docs/testing.md plus this scan.

#[test]
fn merge_to_main_requires_green_tests() {
    let agents = include_str!("../../../AGENTS.md");
    let testing = include_str!("../../../docs/testing.md");
    let std = include_str!("../../../docs/testing-standardization.md");
    let justfile = include_str!("../../../justfile");
    let gitlab = include_str!("../../../.gitlab-ci.yml");

    for (label, src) in [
        ("AGENTS.md", agents),
        ("docs/testing.md", testing),
        ("docs/testing-standardization.md", std),
    ] {
        assert!(
            src.contains("All tests must pass before a merge to main"),
            "{label} must lock a green suite before merge to main"
        );
        assert!(
            src.contains("just test") && src.contains("just test-integration"),
            "{label} must name just test and just test-integration"
        );
        assert!(
            src.contains("All tests must be relevant"),
            "{label} must lock that all tests must be relevant"
        );
        assert!(
            src.contains("If a test is not relevant, decide whether the test or the code should change"),
            "{label} must say a stale test is a product-or-test decision, not a keep-or-silence choice"
        );
    }

    assert!(
        agents.contains("Do not merge unless a human asks you to merge"),
        "AGENTS.md must still require a human merge"
    );
    assert!(
        testing.contains("Do not use `|| true` around `cargo test`"),
        "docs/testing.md must forbid || true around cargo test"
    );

    assert!(
        gitlab.contains("when: never"),
        ".gitlab-ci.yml must stay workflow: rules: when: never"
    );
    assert!(
        !gitlab.contains("script:") && !gitlab.contains("\njob"),
        ".gitlab-ci.yml must not grow a job; tests run locally"
    );

    let integration = justfile
        .split("test-integration:")
        .nth(1)
        .and_then(|s| s.split("\n# ").next())
        .unwrap_or(justfile);
    assert!(
        !integration.contains("process_sandbox") && !integration.contains("plane_runner_tick"),
        "just test-integration must not name deleted orch test binaries:\n{integration}"
    );
    for needle in [
        "store_integration",
        "tick_integration",
        "api_integration",
        "e2e_matrix",
        "host_pool_spawn",
        "host_update_tick",
        "assignable_runner",
        "git_config_patch",
        "host_picker_product",
        "mock_runner",
    ] {
        assert!(
            integration.contains(needle),
            "just test-integration must run {needle}:\n{integration}"
        );
    }
}
