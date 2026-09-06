//! Docs §6.7: seed/create/PATCH/archive persist persona config only.
//! They must not invoke an LLM (no rewrite of role_prompt, no model resolve).
//! Requires DATABASE_URL — never skips.

mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use common::*;

/// Catalog role prompts (docs §9 / `starters()`). Seed must store these
/// verbatim — not an LLM rewrite of the starter copy.
const STARTER_ROLE_PROMPTS: &[(&str, &str)] = &[
    (
        "software-engineer",
        "You are a careful software engineer. Prefer small, reviewable changes, tests, and clear commit messages.",
    ),
    (
        "sales",
        "You are a sales operations specialist. Be concise, CRM-friendly, and never invent customer facts.",
    ),
    (
        "marketing",
        "You are a marketing generalist. Match brand tone; label assumptions; prefer measurable goals.",
    ),
    (
        "customer-support",
        "You are customer support. Empathetic, accurate, never invent product capabilities.",
    ),
    (
        "analyst",
        "You are a data analyst. Cite sources, show uncertainty, prefer tables and clear recommendations.",
    ),
    (
        "devops",
        "You are a DevOps engineer. Prefer reversible changes, observability, and safe rollouts.",
    ),
    (
        "product-discovery",
        "You are a product discovery partner. Separate problem/solution; list risks and open questions.",
    ),
    (
        "content-docs",
        "You are a technical writer. Clear structure, accurate terminology, scannable headings.",
    ),
];

fn persona_by_slug<'a>(out: &'a HttpOut, slug: &str) -> &'a Value {
    out.body["personas"]
        .as_array()
        .expect("personas array")
        .iter()
        .find(|p| p["slug"].as_str() == Some(slug))
        .unwrap_or_else(|| panic!("missing slug {slug}: {}", out.body))
}

async fn signed_in_personas(app: &axum::Router) -> (CookieJar, String) {
    let (_email, jar, login) = signup_and_login(app).await;
    let ws_id = login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id");
    (jar, format!("/api/workspaces/{ws_id}/personas"))
}

async fn seed_starters(app: &axum::Router, jar: &CookieJar, base: &str) -> HttpOut {
    call(
        app,
        "POST",
        &format!("{base}/seed-starters"),
        Some(jar),
        Some(json!({})),
    )
    .await
}

async fn create_persona_with_prompt(
    app: &axum::Router,
    jar: &CookieJar,
    base: &str,
    token: &str,
    role_prompt: &str,
) -> String {
    let created = call(
        app,
        "POST",
        base,
        Some(jar),
        Some(json!({
            "name": format!("No-LLM reviewer {token}"),
            "slug": format!("no-llm-reviewer-{token}"),
            "description": "config only",
            "role_prompt": role_prompt,
            "worker_profile": "coding",
            "model_preference": "auto",
            "tools": ["git", "shell"],
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create fixture: {}",
        created.body
    );
    created.body["id"].as_str().expect("created id").to_string()
}

/// First seed stores docs §9 catalog role_prompt literals (not an LLM rewrite).
#[tokio::test]
async fn seed_starters_stores_catalog_role_prompts_verbatim() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;

    let seeded = seed_starters(&app, &jar, &base).await;
    assert_eq!(seeded.status, StatusCode::OK, "seed: {}", seeded.body);
    assert_eq!(
        seeded.body["inserted"].as_u64(),
        Some(8),
        "first seed must insert the 8 starters: {}",
        seeded.body
    );

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    for (slug, prompt) in STARTER_ROLE_PROMPTS {
        let row = persona_by_slug(&listed, slug);
        assert_eq!(
            row["role_prompt"].as_str(),
            Some(*prompt),
            "seed must store the catalog role_prompt for {slug} verbatim \
             (not LLM-rewritten): {}",
            listed.body
        );
    }
}

/// Create persists the canary role_prompt and model_preference=auto as config.
#[tokio::test]
async fn create_persona_persists_canary_role_prompt_and_model_preference_auto() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;
    let token = Uuid::new_v4().as_simple().to_string();
    let create_prompt = format!("CANARY-CREATE-{token}");

    let created = call(
        &app,
        "POST",
        &base,
        Some(&jar),
        Some(json!({
            "name": format!("No-LLM reviewer {token}"),
            "slug": format!("no-llm-reviewer-{token}"),
            "description": "config only",
            "role_prompt": create_prompt,
            "worker_profile": "coding",
            "model_preference": "auto",
            "tools": ["git", "shell"],
        })),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::CREATED,
        "create: {}",
        created.body
    );
    assert_eq!(
        created.body["role_prompt"].as_str(),
        Some(create_prompt.as_str()),
        "create must persist role_prompt verbatim (not an LLM answer): {}",
        created.body
    );
    assert_eq!(
        created.body["model_preference"].as_str(),
        Some("auto"),
        "create must persist model_preference as config, not resolve a model: {}",
        created.body
    );
}

/// Description-only PATCH must not rewrite role_prompt.
#[tokio::test]
async fn description_only_patch_keeps_role_prompt() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;
    let token = Uuid::new_v4().as_simple().to_string();
    let create_prompt = format!("CANARY-CREATE-{token}");
    let pid = create_persona_with_prompt(&app, &jar, &base, &token, &create_prompt).await;

    let desc_only = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({ "description": "still config only" })),
    )
    .await;
    assert_eq!(
        desc_only.status,
        StatusCode::OK,
        "description-only patch: {}",
        desc_only.body
    );
    assert_eq!(
        desc_only.body["role_prompt"].as_str(),
        Some(create_prompt.as_str()),
        "PATCH of other fields must not rewrite role_prompt: {}",
        desc_only.body
    );
    assert_eq!(
        desc_only.body["description"].as_str(),
        Some("still config only"),
        "{}",
        desc_only.body
    );
}

/// PATCH then archive keep the patched canary role_prompt.
#[tokio::test]
async fn patch_and_archive_keep_patched_canary_role_prompt() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;
    let token = Uuid::new_v4().as_simple().to_string();
    let create_prompt = format!("CANARY-CREATE-{token}");
    let pid = create_persona_with_prompt(&app, &jar, &base, &token, &create_prompt).await;
    let patch_prompt = format!("CANARY-PATCH-{token}");

    let patched = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({
            "role_prompt": patch_prompt,
            "description": "patched config only",
        })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "patch: {}", patched.body);
    assert_eq!(
        patched.body["role_prompt"].as_str(),
        Some(patch_prompt.as_str()),
        "PATCH must persist role_prompt verbatim: {}",
        patched.body
    );

    let archived = call(&app, "DELETE", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(
        archived.status,
        StatusCode::NO_CONTENT,
        "archive: {}",
        archived.body
    );

    let got = call(&app, "GET", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "get archived: {}", got.body);
    assert_eq!(got.body["status"].as_str(), Some("archived"));
    assert_eq!(
        got.body["role_prompt"].as_str(),
        Some(patch_prompt.as_str()),
        "archive must not rewrite role_prompt: {}",
        got.body
    );
}

/// Re-seed after a starter edit must skip (ON CONFLICT DO NOTHING), not upsert.
#[tokio::test]
async fn seed_starters_skips_existing_without_rewriting_role_prompt() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;
    let seeded = seed_starters(&app, &jar, &base).await;
    assert_eq!(seeded.status, StatusCode::OK, "seed: {}", seeded.body);

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let se = persona_by_slug(&listed, "software-engineer");
    let pid = se["id"].as_str().expect("software-engineer id").to_string();
    let token = Uuid::new_v4().as_simple().to_string();
    let canary = format!("CANARY-SEED-SKIP-{token}");

    let patched = call(
        &app,
        "PATCH",
        &format!("{base}/{pid}"),
        Some(&jar),
        Some(json!({ "role_prompt": canary })),
    )
    .await;
    assert_eq!(
        patched.status,
        StatusCode::OK,
        "patch starter: {}",
        patched.body
    );

    let seeded_again = seed_starters(&app, &jar, &base).await;
    assert_eq!(
        seeded_again.status,
        StatusCode::OK,
        "seed again: {}",
        seeded_again.body
    );
    assert_eq!(
        seeded_again.body["skipped"].as_u64(),
        Some(8),
        "second seed must skip existing starters: {}",
        seeded_again.body
    );

    let got = call(&app, "GET", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(got.status, StatusCode::OK, "get after re-seed: {}", got.body);
    assert_eq!(
        got.body["role_prompt"].as_str(),
        Some(canary.as_str()),
        "re-seed must not rewrite a patched starter role_prompt: {}",
        got.body
    );
}

/// First seed on an empty workspace inserts all 8 starters (not skip-all).
#[tokio::test]
async fn seed_starters_first_inserts_eight_on_empty_workspace() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;

    let seeded = seed_starters(&app, &jar, &base).await;
    assert_eq!(seeded.status, StatusCode::OK, "seed: {}", seeded.body);
    assert_eq!(
        seeded.body["inserted"].as_u64(),
        Some(8),
        "first seed on an empty workspace must insert 8: {}",
        seeded.body
    );
    assert_eq!(
        seeded.body["skipped"].as_u64(),
        Some(0),
        "first seed on an empty workspace must skip 0: {}",
        seeded.body
    );
}

/// Seed persists docs §9 worker_profile and tools, not just role_prompt.
#[tokio::test]
async fn seed_starters_persists_docs_section_9_profile_and_tools() {
    let app = app().await;
    let (jar, base) = signed_in_personas(&app).await;

    let seeded = seed_starters(&app, &jar, &base).await;
    assert_eq!(seeded.status, StatusCode::OK, "seed: {}", seeded.body);

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    let row = persona_by_slug(&listed, "software-engineer");
    assert_eq!(
        row["worker_profile"].as_str(),
        Some("coding"),
        "seed must store docs §9 worker_profile for software-engineer: {}",
        listed.body
    );
    assert_eq!(
        row["tools"],
        json!(["git", "shell", "editor"]),
        "seed must store docs §9 tools for software-engineer: {}",
        listed.body
    );
}
