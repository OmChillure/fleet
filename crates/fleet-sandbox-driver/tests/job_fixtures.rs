//! Multi-shape project_setup fixtures (Django + minimal node/shell).
use fleet_sandbox_driver::job::Job;
use fleet_sandbox_driver::policy::{permission_allowed, resolve_named_tool, validate_tool_bin};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sandbox-lab/config")
}

fn load_job(name: &str) -> Value {
    let p = fixtures_dir().join(name);
    let raw = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))
}

fn load_typed_job(name: &str) -> Job {
    let p = fixtures_dir().join(name);
    let raw = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse job {}: {e}", p.display()))
}

#[test]
fn invoices_job_is_default_deny_with_named_tools() {
    let j = load_typed_job("job.123-invoices.json");
    assert!(!j.auto_approve);
    let allow: Vec<&str> = j.permission_allow.iter().map(String::as_str).collect();
    assert_eq!(allow, ["app/", "tests/", "fleet-wsexec", "read"]);
    assert!(
        !allow.iter().any(|a| *a == "*" || *a == "bash" || *a == "sh"),
        "invoices job must not allow *, bash, or sh: {allow:?}"
    );
    let tools = &j.project_setup.as_ref().expect("project_setup").tools;
    assert!(tools.contains_key("django_check"));
    assert!(tools.contains_key("django_migrate"));
    assert!(tools.contains_key("django_test_fleet"));
    assert!(tools.contains_key("django_test"));
}

#[test]
fn invoices_default_json_equals_opencode_alias() {
    assert_eq!(
        load_job("job.123-invoices.json"),
        load_job("job.123-invoices.opencode.json")
    );
}

#[test]
fn invoices_fixture_job_permission_allowed_denies_bash() {
    let j = load_typed_job("job.123-invoices.json");
    assert!(!permission_allowed(
        j.auto_approve,
        &j.permission_allow,
        "bash -lc rm"
    ));
    assert!(permission_allowed(
        j.auto_approve,
        &j.permission_allow,
        "fleet-wsexec django_check"
    ));
}

#[test]
fn invoices_fixture_named_tools_resolve() {
    let j = load_typed_job("job.123-invoices.json");
    let tools = &j.project_setup.as_ref().expect("project_setup").tools;
    let check = resolve_named_tool(tools, "django_check").expect("django_check");
    validate_tool_bin(&check.argv[0]).expect("django_check argv0");
    let migrate = resolve_named_tool(tools, "django_migrate").expect("django_migrate");
    validate_tool_bin(&migrate.argv[0]).expect("django_migrate argv0");
    let fleet = resolve_named_tool(tools, "django_test_fleet").expect("django_test_fleet");
    validate_tool_bin(&fleet.argv[0]).expect("django_test_fleet argv0");
    let test = resolve_named_tool(tools, "django_test").expect("django_test");
    validate_tool_bin(&test.argv[0]).expect("django_test argv0");
}

#[test]
fn invoices_opencode_and_default_match_marker_task() {
    let default = load_job("job.123-invoices.json");
    let opencode = load_job("job.123-invoices.opencode.json");
    assert_eq!(default, opencode);
    assert_eq!(default["harness"], "opencode");
    let prompt = default["prompt"].as_str().unwrap();
    assert!(prompt.contains("fleet_marker"));
    assert!(!prompt.contains("Animal.notes"));
}

#[test]
fn invoices_pi_job_is_model_field_task() {
    let j = load_typed_job("job.123-invoices.pi.json");
    assert_eq!(j.harness, "pi");
    let prompt = j.prompt.as_deref().expect("prompt");
    assert!(prompt.contains("Animal.notes"), "{prompt}");
    assert!(
        !prompt.contains("FLEET_MARKER") && !prompt.contains("Create Python module `app/fleet_marker.py`"),
        "pi job must not assign the OpenCode marker task: {prompt}"
    );
    let allow: Vec<&str> = j.permission_allow.iter().map(String::as_str).collect();
    assert!(
        allow.iter().any(|a| a.contains("invoices/")),
        "{allow:?}"
    );
    assert!(allow.iter().any(|a| a.contains("tests/")), "{allow:?}");
    let tools = &j.project_setup.as_ref().expect("project_setup").tools;
    let fleet = tools.get("django_test_fleet").expect("django_test_fleet");
    assert!(
        fleet
            .argv
            .iter()
            .any(|a| a.contains("test_fleet_animal_notes")),
        "{:?}",
        fleet.argv
    );
}

#[test]
fn invoices_pi_prompt_excludes_fleet_marker() {
    let j = load_job("job.123-invoices.pi.json");
    let prompt = j["prompt"].as_str().unwrap();
    assert!(
        !prompt.contains("FLEET_MARKER")
            && !prompt.contains("Create Python module `app/fleet_marker.py`"),
        "pi prompt must not assign the OpenCode marker task: {prompt}"
    );
}

#[test]
fn invoices_pi_fixture_deserializes_as_job() {
    serde_json::from_value::<Job>(load_job("job.123-invoices.pi.json"))
        .expect("job.123-invoices.pi.json must deserialize as Job");
}

#[test]
fn invoices_pi_allowlist_includes_tests_and_fleet_wsexec() {
    let j = load_typed_job("job.123-invoices.pi.json");
    let allow: Vec<&str> = j.permission_allow.iter().map(String::as_str).collect();
    assert!(allow.iter().any(|a| a.contains("tests/")), "{allow:?}");
    assert!(allow.iter().any(|a| *a == "fleet-wsexec"), "{allow:?}");
}

#[test]
fn invoices_pi_makemigrations_argv_targets_invoices_app() {
    let j = load_typed_job("job.123-invoices.pi.json");
    let argv = &j
        .project_setup
        .as_ref()
        .expect("project_setup")
        .tools
        .get("django_makemigrations")
        .expect("django_makemigrations")
        .argv;
    assert!(argv.iter().any(|a| a == "invoices"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "--name"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "fleet_animal_notes"), "{argv:?}");
}
