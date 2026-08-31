//! Docs §6.7: seed/create/PATCH/archive persist persona config only.
//! They must not invoke an LLM (no rewrite, no completions envelope).
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

fn assert_persona_not_completion(out: &HttpOut, label: &str) {
    assert!(
        out.body.get("choices").is_none(),
        "{label} must not be a chat-completions payload: {}",
        out.body
    );
    assert!(
        out.body.get("usage").is_none(),
        "{label} must not include LLM usage: {}",
        out.body
    );
    assert!(
        out.body.pointer("/message/content").is_none(),
        "{label} must not be a chat message: {}",
        out.body
    );
    assert_ne!(
        out.body["object"].as_str(),
        Some("chat.completion"),
        "{label} must not be a chat.completion object: {}",
        out.body
    );
}

/// Seed / create / PATCH / archive persist the role prompt verbatim.
/// They must succeed without calling an LLM.
#[tokio::test]
async fn persona_seed_create_patch_archive_do_not_invoke_llm() {
    let app = app().await;
    let (_email, jar, login) = signup_and_login(&app).await;
    let ws_id = login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id");
    let base = format!("/api/workspaces/{ws_id}/personas");

    let seeded = call(
        &app,
        "POST",
        &format!("{base}/seed-starters"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(seeded.status, StatusCode::OK, "seed: {}", seeded.body);
    assert_eq!(
        seeded.body["inserted"].as_u64().unwrap_or(0)
            + seeded.body["skipped"].as_u64().unwrap_or(0),
        8,
        "seed must insert-if-absent the 8 starters, not generate them: {}",
        seeded.body
    );
    assert_persona_not_completion(&seeded, "seed-starters");

    // Second seed must skip, not call an LLM to rewrite the catalog.
    let seeded_again = call(
        &app,
        "POST",
        &format!("{base}/seed-starters"),
        Some(&jar),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        seeded_again.status,
        StatusCode::OK,
        "seed again: {}",
        seeded_again.body
    );
    assert_eq!(
        seeded_again.body["inserted"].as_u64(),
        Some(0),
        "second seed must insert-if-absent (not regenerate): {}",
        seeded_again.body
    );
    assert_eq!(
        seeded_again.body["skipped"].as_u64(),
        Some(8),
        "second seed must skip the 8 starters: {}",
        seeded_again.body
    );
    assert_persona_not_completion(&seeded_again, "seed-starters again");

    let listed = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "list: {}", listed.body);
    assert_persona_not_completion(&listed, "list");
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

    let token = Uuid::new_v4().as_simple().to_string();
    let create_prompt = format!(
        "CANARY-CREATE-{token}: What is 2+2? Reply with only the number. Store this verbatim; do not complete."
    );
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
    assert_persona_not_completion(&created, "create");
    assert_eq!(
        created.body["role_prompt"].as_str(),
        Some(create_prompt.as_str()),
        "create must persist role_prompt verbatim (not an LLM answer): {}",
        created.body
    );
    assert_ne!(
        created.body["role_prompt"].as_str().map(str::trim),
        Some("4"),
        "create must not complete the canary question: {}",
        created.body
    );
    assert_eq!(
        created.body["model_preference"].as_str(),
        Some("auto"),
        "create must persist model_preference as config, not resolve a model: {}",
        created.body
    );
    let pid = created.body["id"].as_str().expect("created id").to_string();

    let got_created = call(&app, "GET", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(
        got_created.status,
        StatusCode::OK,
        "get created: {}",
        got_created.body
    );
    assert_persona_not_completion(&got_created, "get created");
    assert_eq!(
        got_created.body["role_prompt"].as_str(),
        Some(create_prompt.as_str()),
        "GET after create must still be the canary, not an LLM rewrite: {}",
        got_created.body
    );

    let listed_after_create = call(&app, "GET", &base, Some(&jar), None).await;
    assert_eq!(
        listed_after_create.status,
        StatusCode::OK,
        "list after create: {}",
        listed_after_create.body
    );
    assert_eq!(
        persona_by_slug(&listed_after_create, &format!("no-llm-reviewer-{token}"))["role_prompt"]
            .as_str(),
        Some(create_prompt.as_str()),
        "list after create must keep the canary: {}",
        listed_after_create.body
    );

    // Description-only PATCH must not "improve" or complete the canary.
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
    assert_persona_not_completion(&desc_only, "description-only patch");
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

    let patch_prompt = format!("CANARY-PATCH-{token}: still config; still not a completion.");
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
    assert_persona_not_completion(&patched, "patch");
    assert_eq!(
        patched.body["role_prompt"].as_str(),
        Some(patch_prompt.as_str()),
        "PATCH must persist role_prompt verbatim: {}",
        patched.body
    );
    assert_eq!(
        patched.body["description"].as_str(),
        Some("patched config only"),
        "{}",
        patched.body
    );

    let got_patched = call(&app, "GET", &format!("{base}/{pid}"), Some(&jar), None).await;
    assert_eq!(
        got_patched.status,
        StatusCode::OK,
        "get patched: {}",
        got_patched.body
    );
    assert_persona_not_completion(&got_patched, "get patched");
    assert_eq!(
        got_patched.body["role_prompt"].as_str(),
        Some(patch_prompt.as_str()),
        "GET after PATCH must still be the canary, not an LLM rewrite: {}",
        got_patched.body
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
    assert_persona_not_completion(&got, "get archived");
    assert_eq!(got.body["status"].as_str(), Some("archived"));
    assert_eq!(
        got.body["role_prompt"].as_str(),
        Some(patch_prompt.as_str()),
        "archive must not rewrite role_prompt: {}",
        got.body
    );
}
