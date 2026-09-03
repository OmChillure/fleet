//! Multi-shape project_setup fixtures (Django + minimal node/shell).
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

#[test]
fn invoices_job_is_default_deny_with_named_tools() {
    let j = load_job("job.123-invoices.json");
    assert_eq!(j["auto_approve"], false);
    // Explicit allowlist: project folders + tools — never auto_approve, no "*"
    let allow = j["permission_allow"].as_array().expect("permission_allow");
    assert!(!allow.is_empty());
    assert!(
        !allow.iter().any(|v| v.as_str() == Some("*")),
        "invoices job must not use permission_allow \"*\""
    );
    let allow_s: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
    assert!(allow_s.iter().any(|a| a.contains("app/")), "{allow_s:?}");
    assert!(allow_s.iter().any(|a| a.contains("tests/")), "{allow_s:?}");
    assert!(
        allow_s.iter().any(|a| a.contains("fleet-wsexec")),
        "need fleet-wsexec on allowlist: {allow_s:?}"
    );
    assert!(
        !allow_s.iter().any(|a| *a == "bash" || *a == "sh"),
        "bash/sh must not be on permission_allow: {allow_s:?}"
    );
    let tools = &j["project_setup"]["tools"];
    assert!(tools.get("django_check").is_some());
    assert!(tools.get("django_migrate").is_some());
    assert!(tools.get("django_test_fleet").is_some());
    assert!(tools.get("django_test").is_some());
    assert_eq!(
        tools["django_check"]["argv"],
        serde_json::json!(["python", "manage.py", "check"])
    );
    assert_eq!(
        tools["django_test_fleet"]["argv"],
        serde_json::json!([
            "python",
            "manage.py",
            "test",
            "tests.test_fleet_smoke",
            "--verbosity",
            "2"
        ])
    );
    assert_eq!(
        tools["django_test"]["argv"],
        serde_json::json!(["python", "manage.py", "test", "--verbosity", "1"])
    );
    let prompt = j["prompt"].as_str().unwrap();
    assert!(prompt.contains("django_test_fleet"));
    assert!(prompt.contains("django_test"));
    assert!(prompt.contains("fleet_marker"));
    assert!(prompt.contains("times out") || prompt.contains("timeout"));
    assert!(
        j["project_setup"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("pip install"))
    );
    assert!(j["repo_url"].as_str().unwrap().contains("123-invoices"));
}

#[test]
fn invoices_opencode_and_default_match_marker_task() {
    for name in ["job.123-invoices.json", "job.123-invoices.opencode.json"] {
        let j = load_job(name);
        assert_eq!(j["harness"], "opencode", "{name}");
        let prompt = j["prompt"].as_str().unwrap();
        assert!(prompt.contains("fleet_marker"), "{name}");
        assert!(!prompt.contains("Animal.notes"), "{name}");
        let allow: Vec<&str> = j["permission_allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(allow.iter().any(|a| a.contains("app/")), "{allow:?}");
        assert!(!allow.iter().any(|a| *a == "*"), "{allow:?}");
    }
}

#[test]
fn invoices_pi_job_is_model_field_task() {
    let j = load_job("job.123-invoices.pi.json");
    assert_eq!(j["harness"], "pi");
    assert_eq!(j["auto_approve"], false);
    let allow = j["permission_allow"].as_array().unwrap();
    assert!(!allow.iter().any(|v| v.as_str() == Some("*")));
    let allow_s: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        allow_s.iter().any(|a| a.contains("invoices/")),
        "{allow_s:?}"
    );
    let tools = &j["project_setup"]["tools"];
    assert!(tools.get("django_makemigrations").is_some());
    assert!(tools.get("django_test_fleet").is_some());
    assert_eq!(
        tools["django_test_fleet"]["argv"],
        serde_json::json!([
            "python",
            "manage.py",
            "test",
            "tests.test_fleet_animal_notes",
            "--verbosity",
            "2"
        ])
    );
    let prompt = j["prompt"].as_str().unwrap();
    assert!(prompt.contains("Animal") || prompt.contains("animal"));
    assert!(prompt.contains("notes"));
    assert!(prompt.contains("makemigrations") || tools.get("django_makemigrations").is_some());
    assert!(j["provider"]["base_url"].as_str().is_some());
}

#[test]
fn minimal_node_job_shape_differs_from_django() {
    let j = load_job("job.minimal-node.json");
    assert_eq!(j["auto_approve"], false);
    let allow: Vec<&str> = j["permission_allow"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(!allow.iter().any(|a| *a == "*"), "{allow:?}");
    assert!(allow.iter().any(|a| a.contains("docs/")), "{allow:?}");
    assert!(allow.iter().any(|a| *a == "fleet-wsexec"), "{allow:?}");
    let tools = &j["project_setup"]["tools"];
    assert!(tools.get("list_root").is_some());
    assert!(tools.get("django_check").is_none());
    assert_eq!(tools["list_root"]["argv"][0], "ls");
    assert!(
        !j["project_setup"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("manage.py"))
    );
}

#[test]
fn deny_default_fixture_empty_allow() {
    let j = load_job("job.deny-default.example.json");
    assert_eq!(j["auto_approve"], false);
    assert_eq!(j["permission_allow"], serde_json::json!([]));
}

#[test]
fn both_setup_shapes_have_strategy_commands() {
    for name in ["job.123-invoices.json", "job.minimal-node.json"] {
        let j = load_job(name);
        assert_eq!(j["project_setup"]["strategy"], "commands", "{name}");
        assert!(
            j["project_setup"]["tools"]
                .as_object()
                .map(|o| !o.is_empty())
                .unwrap_or(false),
            "{name} tools non-empty"
        );
    }
}

#[test]
fn commands_only_shape_empty_tools_default_deny() {
    let j = load_job("job.commands-only.json");
    assert_eq!(j["auto_approve"], false);
    assert_eq!(j["permission_allow"], serde_json::json!([]));
    assert_eq!(j["project_setup"]["strategy"], "commands");
    let tools = j["project_setup"]["tools"].as_object().unwrap();
    assert!(tools.is_empty(), "commands-only has no named tools");
    assert!(
        !j["project_setup"]["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
