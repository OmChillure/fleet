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
    if key.is_empty() || key == "default" || key == "worker" || key == "generalist" || key == "specialist" {
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
        return all
            .iter()
            .find(|p| p.slug.contains("reviewer"))
            .cloned();
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
            all.iter().find(|p| {
                normalize(&p.slug).contains(&key) || normalize(&p.name).contains(&key)
            })
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
            body: BUILTIN_CODING.into(),
        },
        Persona {
            slug: "engineering-code-reviewer".into(),
            name: "Code Reviewer".into(),
            description: "Reviews diffs for correctness, security, and maintainability".into(),
            division: "engineering".into(),
            emoji: "•".into(),
            vibe: "Finds the three issues that actually matter.".into(),
            body: BUILTIN_REVIEWER.into(),
        },
        Persona {
            slug: "engineering-frontend-developer".into(),
            name: "Frontend Developer".into(),
            description: "Implements accessible, performant UI".into(),
            division: "engineering".into(),
            emoji: "•".into(),
            vibe: "Pixel-aware, accessibility-first.".into(),
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

    #[test]
    fn frontmatter_parse() {
        let (m, body) = split_frontmatter(
            "---\nname: Frontend Developer\ndescription: UI\nemoji: x\nvibe: fast\n---\n\n# Hello\n",
        );
        assert_eq!(m.get("name").unwrap(), "Frontend Developer");
        assert!(body.contains("# Hello"));
    }

    #[test]
    fn pick_aliases() {
        let all = builtin_personas();
        assert!(pick(&all, "reviewer").unwrap().slug.contains("reviewer"));
        assert!(pick(&all, "frontend").unwrap().slug.contains("frontend"));
        assert!(pick(&all, "coding").is_some());
        assert_eq!(pick(&all, "default").unwrap().slug, "generalist");
        assert_eq!(pick(&all, "").unwrap().slug, "generalist");
        assert_eq!(pick(&all, "generalist").unwrap().slug, "generalist");
        assert_eq!(
            pick(&all, "engineering-code-reviewer").unwrap().slug,
            "engineering-code-reviewer"
        );
        assert!(pick(&all, "no-such-persona").is_none());
    }

    #[test]
    fn title_case_slug() {
        assert_eq!(title_case("engineering-backend-architect"), "Engineering Backend Architect");
        assert_eq!(title_case(""), "");
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

    #[tokio::test]
    async fn resolve_uses_personas_dir_without_network() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let testing = dir.path().join("testing");
        std::fs::create_dir_all(&testing).unwrap();
        std::fs::write(
            testing.join("qa-specialist.md"),
            "---\nname: QA Specialist\n---\n\nTest everything.\n",
        )
        .unwrap();

        let prev = std::env::var("FLEET_PERSONAS_DIR").ok();
        // SAFETY: test-only env; restored below.
        unsafe { std::env::set_var("FLEET_PERSONAS_DIR", dir.path()) };
        let p = resolve("tester", dir.path()).await;
        match prev {
            Some(v) => unsafe { std::env::set_var("FLEET_PERSONAS_DIR", v) },
            None => unsafe { std::env::remove_var("FLEET_PERSONAS_DIR") },
        }
        assert_eq!(p.slug, "qa-specialist");
        assert_eq!(p.division, "testing");
        assert!(p.body.contains("Test everything"));
    }
}
