//! Agency-agents personas (https://github.com/msitarzewski/agency-agents).
//! Cloned into DATA_DIR/agency-agents (or FLEET_PERSONAS_DIR). `worker_profile` selects one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

const UPSTREAM: &str = "https://github.com/msitarzewski/agency-agents.git";
const SYNC_TTL: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Serialize)]
pub struct Persona {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub division: String,
    pub emoji: String,
    pub vibe: String,
    /// Workspace catalog `model_preference`. Empty for roster personas.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_preference: String,
    /// Full markdown body (frontmatter stripped + original).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonaSummary {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub division: String,
    pub emoji: String,
    pub vibe: String,
}

impl Persona {
    pub fn summary(&self) -> PersonaSummary {
        PersonaSummary {
            slug: self.slug.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            division: self.division.clone(),
            emoji: self.emoji.clone(),
            vibe: self.vibe.clone(),
        }
    }
}

static LAST_SYNC: Mutex<Option<Instant>> = Mutex::new(None);

pub fn personas_dir(data_dir: &Path) -> PathBuf {
    std::env::var("FLEET_PERSONAS_DIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("agency-agents"))
}

/// Clone or refresh the upstream roster (best-effort).
pub async fn ensure_roster(dir: &Path) -> Result<(), String> {
    if let Ok(g) = LAST_SYNC.lock()
        && let Some(t) = *g
        && t.elapsed() < SYNC_TTL
        && dir.join(".git").exists()
    {
        return Ok(());
    }

    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || sync_blocking(&dir))
        .await
        .map_err(|e| e.to_string())??;

    if let Ok(mut g) = LAST_SYNC.lock() {
        *g = Some(Instant::now());
    }
    Ok(())
}

fn sync_blocking(dir: &Path) -> Result<(), String> {
    if dir.join(".git").exists() {
        if std::env::var("FLEET_PERSONAS_UPDATE")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
        {
            let st = std::process::Command::new("git")
                .args(["-C", &dir.to_string_lossy(), "pull", "--ff-only", "--quiet"])
                .status()
                .map_err(|e| format!("git pull personas: {e}"))?;
            if !st.success() {
                tracing::warn!("git pull agency-agents failed; using existing clone");
            }
        }
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    let st = std::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--quiet",
            UPSTREAM,
            &dir.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("git clone agency-agents: {e}"))?;
    if !st.success() {
        return Err("git clone agency-agents failed (network?)".into());
    }
    tracing::info!(dir = %dir.display(), "cloned agency-agents personas");
    Ok(())
}

pub async fn list_personas(data_dir: &Path) -> Vec<PersonaSummary> {
    let dir = personas_dir(data_dir);
    let _ = ensure_roster(&dir).await;
    let mut all = scan_dir(&dir);
    if all.is_empty() {
        all = builtin_personas();
    }
    let mut out: Vec<PersonaSummary> = all.into_iter().map(|p| p.summary()).collect();
    out.sort_by(|a, b| a.division.cmp(&b.division).then(a.name.cmp(&b.name)));
    out
}

/// Workspace catalog first (role_prompt + model_preference), then the
/// agency-agents roster. The personas HTTP API never invokes an LLM;
/// this is consumption at spawn.
pub async fn resolve_for_workspace(
    pool: &sqlx::PgPool,
    workspace_id: uuid::Uuid,
    profile: &str,
    data_dir: &Path,
) -> Persona {
    match fleet_db::repos::personas::find_by_slug(pool, workspace_id, profile).await {
        Ok(Some(row)) => return persona_from_workspace_row(&row),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!(
                error = %e,
                workspace_id = %workspace_id,
                profile,
                "workspace persona lookup failed; using roster"
            );
        }
    }
    resolve(profile, data_dir).await
}

fn persona_from_workspace_row(row: &fleet_db::Persona) -> Persona {
    let role = row.role_prompt.trim();
    let body = if role.is_empty() {
        format!("# {}\n", row.name.trim())
    } else {
        format!("# {}\n\n{}\n", row.name.trim(), role)
    };
    Persona {
        slug: row.slug.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        division: "workspace".into(),
        emoji: row.icon.clone(),
        vibe: String::new(),
        model_preference: row.model_preference.clone().unwrap_or_default(),
        body,
    }
}

/// Resolve `worker_profile` to a full persona (body included).
pub async fn resolve(profile: &str, data_dir: &Path) -> Persona {
    let dir = personas_dir(data_dir);
    let _ = ensure_roster(&dir).await;
    let all = {
        let scanned = scan_dir(&dir);
        if scanned.is_empty() {
            builtin_personas()
        } else {
            scanned
        }
    };
    pick(&all, profile).unwrap_or_else(fallback_persona)
}

fn pick(all: &[Persona], profile: &str) -> Option<Persona> {
    let key = normalize(profile);
    if key.is_empty()
        || key == "default"
        || key == "worker"
        || key == "generalist"
        || key == "specialist"
    {
        return all
            .iter()
            .find(|p| p.slug == "generalist")
            .cloned()
            .or_else(|| all.first().cloned());
    }
    // Optional shortcuts — not defaults. Explicit "coding" still maps to an engineer.
    if key == "coding" || key == "engineer" || key == "backend" {
        return all
            .iter()
            .find(|p| p.slug.contains("backend-architect") || p.slug.contains("ai-engineer"))
            .cloned();
    }
    if key == "reviewer" || key == "code-reviewer" || key == "review" {
        return all.iter().find(|p| p.slug.contains("reviewer")).cloned();
    }
    if key == "frontend" || key == "ui" {
        return all
            .iter()
            .find(|p| p.slug.contains("frontend-developer"))
            .cloned();
    }
    if key == "tester" || key == "qa" || key == "testing" {
        return all.iter().find(|p| p.division == "testing").cloned();
    }
    all.iter()
        .find(|p| normalize(&p.slug) == key)
        .or_else(|| all.iter().find(|p| normalize(&p.slug).ends_with(&key)))
        .or_else(|| all.iter().find(|p| normalize(&p.name) == key))
        .or_else(|| {
            all.iter()
                .find(|p| normalize(&p.slug).contains(&key) || normalize(&p.name).contains(&key))
        })
        .cloned()
}

fn normalize(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .replace(' ', "-")
}

fn scan_dir(root: &Path) -> Vec<Persona> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    walk_md(root, root, &mut files);
    files
}

fn walk_md(root: &Path, dir: &Path, out: &mut Vec<Persona>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if matches!(
                name.as_str(),
                "integrations" | "examples" | "scripts" | ".github" | "strategy"
            ) {
                continue;
            }
            walk_md(root, &p, out);
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        if matches!(
            name.as_str(),
            "README.md" | "CONTRIBUTING.md" | "SECURITY.md" | "LICENSE.md"
        ) {
            continue;
        }
        if let Some(persona) = parse_file(root, &p) {
            out.push(persona);
        }
    }
}

fn parse_file(root: &Path, path: &Path) -> Option<Persona> {
    let raw = std::fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let division = path
        .strip_prefix(root)
        .ok()
        .and_then(|r| r.iter().next())
        .and_then(|s| s.to_str())
        .unwrap_or("specialized")
        .to_string();
    let (meta, body) = split_frontmatter(&raw);
    Some(Persona {
        slug: stem.clone(),
        name: meta
            .get("name")
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| title_case(&stem)),
        description: meta.get("description").cloned().unwrap_or_default(),
        division,
        emoji: meta.get("emoji").cloned().unwrap_or_else(|| "•".into()),
        vibe: meta.get("vibe").cloned().unwrap_or_default(),
        model_preference: String::new(),
        body: if body.trim().is_empty() { raw } else { body },
    })
}

fn split_frontmatter(raw: &str) -> (HashMap<String, String>, String) {
    let mut map = HashMap::new();
    let t = raw.trim_start_matches('\u{feff}');
    if !t.starts_with("---") {
        return (map, raw.to_string());
    }
    let rest = t.strip_prefix("---").unwrap_or(t);
    let Some((fm, body)) = rest.split_once("\n---") else {
        return (map, raw.to_string());
    };
    for line in fm.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            map.insert(k.trim().to_ascii_lowercase(), v);
        }
    }
    (map, body.trim_start_matches('\n').to_string())
}

fn title_case(stem: &str) -> String {
    stem.split('-')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn fallback_persona() -> Persona {
    Persona {
        slug: "generalist".into(),
        name: "Specialist".into(),
        description: "Completes the assigned task with teammates".into(),
        division: "specialized".into(),
        emoji: "•".into(),
        vibe: "Does the job, stays in lane, hands off clearly.".into(),
        model_preference: String::new(),
        body: BUILTIN_GENERALIST.into(),
    }
}

fn builtin_personas() -> Vec<Persona> {
    vec![
        fallback_persona(),
        Persona {
            slug: "engineering-backend-architect".into(),
            name: "Backend Architect".into(),
            description: "Designs and implements reliable backend systems".into(),
            division: "engineering".into(),
            emoji: "•".into(),
            vibe: "Ships durable APIs and clear module boundaries.".into(),
            model_preference: String::new(),
            body: BUILTIN_CODING.into(),
        },
        Persona {
            slug: "engineering-code-reviewer".into(),
            name: "Code Reviewer".into(),
            description: "Reviews diffs for correctness, security, and maintainability".into(),
            division: "engineering".into(),
            emoji: "•".into(),
            vibe: "Finds the three issues that actually matter.".into(),
            model_preference: String::new(),
            body: BUILTIN_REVIEWER.into(),
        },
        Persona {
            slug: "engineering-frontend-developer".into(),
            name: "Frontend Developer".into(),
            description: "Implements accessible, performant UI".into(),
            division: "engineering".into(),
            emoji: "•".into(),
            vibe: "Pixel-aware, accessibility-first.".into(),
            model_preference: String::new(),
            body: BUILTIN_FRONTEND.into(),
        },
    ]
}

const BUILTIN_GENERALIST: &str = r#"# Specialist

You are a competent specialist on a multi-agent team. Read the assigned task,
inspect the shared workspace, do only your slice, and coordinate with teammates
when you need something they own.

## Rules
- Stay inside the assigned work directory
- Use `work/` for shared artifacts the next agent will inherit
- Write `result.md` summarizing what you did and what is still open
"#;

const BUILTIN_CODING: &str = r#"# Backend / software engineer

You are a senior software engineer. Read the task spec, inspect the workspace,
implement the smallest correct change, and verify with tests or a dry run.

## Rules
- Stay inside the assigned work directory
- Prefer existing patterns in the workspace
- Write `result.md` summarizing what you did
"#;

const BUILTIN_REVIEWER: &str = r#"# Code reviewer

You are a rigorous code reviewer. Read the task and workspace, list concrete
issues (correctness, security, tests), and write `result.md` with findings.
Do not rewrite the whole project — review what is in front of you.
"#;

const BUILTIN_FRONTEND: &str = r#"# Frontend developer

You are a frontend engineer. Implement UI from the task spec with accessible
markup, sensible structure, and no unnecessary dependencies. Write `result.md`.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_from(all: &[Persona], profile: &str) -> Persona {
        pick(all, profile).unwrap_or_else(fallback_persona)
    }

    #[test]
    fn reviewer_code_reviewer_review_resolve_to_code_reviewer() {
        let all = builtin_personas();
        for alias in ["reviewer", "code-reviewer", "review"] {
            let got = resolve_from(&all, alias);
            assert_eq!(got.slug, "engineering-code-reviewer", "{alias:?}");
        }
    }

    #[test]
    fn frontend_ui_resolve_to_frontend_developer() {
        let all = builtin_personas();
        for alias in ["frontend", "ui"] {
            let got = resolve_from(&all, alias);
            assert_eq!(got.slug, "engineering-frontend-developer", "{alias:?}");
        }
    }

    #[test]
    fn default_empty_worker_generalist_specialist_resolve_to_generalist() {
        let all = builtin_personas();
        for alias in ["default", "", "worker", "generalist", "specialist"] {
            let got = resolve_from(&all, alias);
            assert_eq!(got.slug, "generalist", "{alias:?}");
        }
    }

    #[test]
    fn exact_roster_slug_selects_that_persona() {
        let all = builtin_personas();
        let got = resolve_from(&all, "engineering-code-reviewer");
        assert_eq!(got.slug, "engineering-code-reviewer");
    }

    #[test]
    fn coding_engineer_backend_resolve_to_backend_architect() {
        let all = builtin_personas();
        for alias in ["coding", "engineer", "backend"] {
            let got = resolve_from(&all, alias);
            assert_eq!(got.slug, "engineering-backend-architect", "{alias:?}");
        }
    }

    #[test]
    fn unknown_profile_resolve_is_fallback_generalist() {
        let all = builtin_personas();
        let got = resolve_from(&all, "no-such-persona");
        assert_eq!(got.slug, "generalist");
    }

    #[test]
    fn tester_qa_testing_picks_first_testing_division() {
        let mut all = builtin_personas();
        all.push(Persona {
            slug: "e2e-specialist".into(),
            name: "E2E Specialist".into(),
            description: "Runs the planted checks".into(),
            division: "testing".into(),
            emoji: "•".into(),
            vibe: String::new(),
            model_preference: String::new(),
            body: String::new(),
        });
        for alias in ["tester", "qa", "testing"] {
            let got = resolve_from(&all, alias);
            assert_eq!(got.slug, "e2e-specialist", "{alias:?}");
            assert_eq!(got.division, "testing", "{alias:?}");
        }
    }

    #[test]
    fn scan_dir_omitted_name_is_title_cased_stem() {
        let dir = tempfile::tempdir().unwrap();
        let eng = dir.path().join("engineering");
        std::fs::create_dir_all(&eng).unwrap();
        std::fs::write(
            eng.join("engineering-backend-architect.md"),
            "---\ndescription: APIs\n---\n\n# Body\n",
        )
        .unwrap();

        let all = scan_dir(dir.path());
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].slug, "engineering-backend-architect");
        assert_eq!(all[0].name, "Engineering Backend Architect");
    }

    #[test]
    fn scan_dir_empty_frontmatter_name_is_title_cased_stem() {
        let dir = tempfile::tempdir().unwrap();
        let eng = dir.path().join("engineering");
        std::fs::create_dir_all(&eng).unwrap();
        std::fs::write(
            eng.join("engineering-backend-architect.md"),
            "---\nname: \"\"\ndescription: APIs\n---\n\n# Body\n",
        )
        .unwrap();

        let all = scan_dir(dir.path());
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].slug, "engineering-backend-architect");
        assert_eq!(all[0].name, "Engineering Backend Architect");
    }

    #[test]
    fn title_case_drops_empty_hyphen_segments() {
        let dir = tempfile::tempdir().unwrap();
        let testing = dir.path().join("testing");
        std::fs::create_dir_all(&testing).unwrap();
        std::fs::write(
            testing.join("--qa-specialist-.md"),
            "---\ndescription: QA\n---\n\n# Body\n",
        )
        .unwrap();

        let all = scan_dir(dir.path());
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].slug, "--qa-specialist-");
        assert_eq!(all[0].name, "Qa Specialist");
    }

    #[test]
    fn scan_dir_reads_frontmatter_and_skips_readme() {
        let dir = tempfile::tempdir().unwrap();
        let eng = dir.path().join("engineering");
        std::fs::create_dir_all(&eng).unwrap();
        std::fs::write(
            eng.join("engineering-backend-architect.md"),
            "---\nname: Backend Architect\ndescription: APIs\nemoji: x\nvibe: ships\n---\n\n# Body\n",
        )
        .unwrap();
        std::fs::write(eng.join("README.md"), "# ignore me\n").unwrap();
        std::fs::create_dir_all(dir.path().join("examples")).unwrap();
        std::fs::write(dir.path().join("examples").join("sample.md"), "# no\n").unwrap();

        let all = scan_dir(dir.path());
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].slug, "engineering-backend-architect");
        assert_eq!(all[0].name, "Backend Architect");
        assert_eq!(all[0].division, "engineering");
        assert!(all[0].body.contains("# Body"));
    }

    fn plant_roster_persona(root: &Path, division: &str, slug: &str, body: &str) {
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let dir = root.join(division);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{slug}.md")),
            format!("---\nname: {slug}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    fn personas_dir_env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    struct PersonasDirEnv {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }

    impl PersonasDirEnv {
        async fn lock_and_set(value: Option<&Path>) -> Self {
            let lock = personas_dir_env_lock().lock().await;
            let prev = std::env::var("FLEET_PERSONAS_DIR").ok();
            set_personas_dir_env(value);
            Self { _lock: lock, prev }
        }
    }

    impl Drop for PersonasDirEnv {
        fn drop(&mut self) {
            set_personas_dir_env(self.prev.as_deref().map(Path::new));
        }
    }

    fn set_personas_dir_env(value: Option<&Path>) {
        // SAFETY: caller holds personas_dir_env_lock; Drop restores prior value.
        unsafe {
            match value {
                Some(v) => std::env::set_var("FLEET_PERSONAS_DIR", v),
                None => std::env::remove_var("FLEET_PERSONAS_DIR"),
            }
        }
    }

    #[tokio::test]
    async fn resolve_uses_personas_dir_without_network() {
        let env_dir = tempfile::tempdir().unwrap();
        plant_roster_persona(
            env_dir.path(),
            "testing",
            "qa-specialist",
            "from-FLEET_PERSONAS_DIR",
        );

        let data_dir = tempfile::tempdir().unwrap();
        plant_roster_persona(
            &data_dir.path().join("agency-agents"),
            "testing",
            "data-dir-decoy",
            "from-data_dir",
        );

        let _env = PersonasDirEnv::lock_and_set(Some(env_dir.path())).await;
        assert_eq!(personas_dir(data_dir.path()).as_path(), env_dir.path());
        let p = resolve("tester", data_dir.path()).await;
        assert_eq!(p.slug, "qa-specialist");
        assert_eq!(p.division, "testing");
        assert!(p.body.contains("from-FLEET_PERSONAS_DIR"));
        assert!(!p.body.contains("from-data_dir"));
    }

    #[tokio::test]
    async fn resolve_without_env_reads_data_dir_agency_agents() {
        let data_dir = tempfile::tempdir().unwrap();
        plant_roster_persona(
            &data_dir.path().join("agency-agents"),
            "testing",
            "qa-specialist",
            "from-data_dir-agency-agents",
        );

        let _env = PersonasDirEnv::lock_and_set(None).await;
        assert_eq!(
            personas_dir(data_dir.path()),
            data_dir.path().join("agency-agents")
        );
        let p = resolve("tester", data_dir.path()).await;
        assert_eq!(p.slug, "qa-specialist");
        assert_eq!(p.division, "testing");
        assert!(p.body.contains("from-data_dir-agency-agents"));
    }

    #[tokio::test]
    async fn resolve_unknown_or_empty_scan_falls_back_to_generalist() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(data_dir.path().join("agency-agents").join(".git")).unwrap();

        let _env = PersonasDirEnv::lock_and_set(None).await;
        let p = resolve("no-such-persona", data_dir.path()).await;
        assert_eq!(p.slug, "generalist");
        assert_eq!(p.body, BUILTIN_GENERALIST);
    }
}
