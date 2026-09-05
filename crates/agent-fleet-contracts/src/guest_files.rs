//! Text files that travel between the plane and a runner guest.
//!
//! Spawn packs these into `spawn_session.workspace_files`. The guest writes
//! them back as status `files` so the next specialist can see `work/` and
//! `result.md`. Paths are relative; `..` is refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub const MAX_GUEST_FILES: usize = 32;
pub const MAX_GUEST_FILE_CHARS: usize = 64 * 1024;
pub const MAX_GUEST_TOTAL_CHARS: usize = 512 * 1024;

const ROOT_NAMES: &[&str] = &[
    "TEAM.md",
    "RUN.md",
    "PERSONA.md",
    "task.md",
    "prompt.md",
    "spec.md",
    "TEAM_CHAT.md",
    ".team.json",
    "fleet-tool.sh",
    "result.md",
];

/// Shell helper the guest (or `grok`/`claude` bash) uses to call the plane.
pub const FLEET_TOOL_SH: &str = r#"#!/bin/sh
# Call the control plane as this worker. Tokens stay on the plane.
set -eu
base="${FLEET_ORCHESTRATOR_URL:-${FLEET_API_BASE:-}}"
token="${FLEET_WORKER_TOKEN:-}"
project="${FLEET_PROJECT_ID:-}"
session="${FLEET_SESSION_ID:-}"
if [ -z "$base" ] || [ -z "$token" ] || [ -z "$project" ] || [ -z "$session" ]; then
  echo "fleet-tool: missing FLEET_ORCHESTRATOR_URL / FLEET_WORKER_TOKEN / FLEET_PROJECT_ID / FLEET_SESSION_ID" >&2
  exit 2
fi
base="${base%/}"
url="$base/v1/projects/$project/workers/$session/io"
op="${1:-}"
shift || true
case "$op" in
  integration)
    provider="${1:-}"; operation="${2:-}"; args="${3:-{}}"
    body=$(printf '{"op":"integration","provider":"%s","operation":"%s","args":%s}' "$provider" "$operation" "$args")
    ;;
  chat-post)
    to="${1:-*}"; body_text="${2:-}"
    body=$(printf '{"op":"team_chat_post","to":"%s","kind":"say","body":"%s"}' "$to" "$body_text")
    ;;
  chat-list)
    body='{"op":"team_chat_list","all":true}'
    ;;
  request-changes)
    reason="${1:-}"
    body=$(printf '{"op":"request_changes","reason":"%s"}' "$reason")
    ;;
  *)
    echo "usage: fleet-tool integration PROVIDER OPERATION [ARGS_JSON]" >&2
    echo "       fleet-tool chat-post TO BODY" >&2
    echo "       fleet-tool chat-list" >&2
    echo "       fleet-tool request-changes REASON" >&2
    exit 2
    ;;
esac
exec curl -fsS -X POST "$url" \
  -H "Authorization: Bearer $token" \
  -H "Content-Type: application/json" \
  -d "$body"
"#;

pub fn safe_rel_path(rel: &str) -> Option<String> {
    let rel = rel.trim().trim_start_matches(['/', '\\']).replace('\\', "/");
    if rel.is_empty() {
        return None;
    }
    if rel.split('/').any(|p| p == ".." || p.is_empty() || p == ".") {
        return None;
    }
    Some(rel)
}

pub fn should_pack_rel(rel: &str) -> bool {
    ROOT_NAMES.iter().any(|n| *n == rel)
        || rel.starts_with("handoff/")
        || rel.starts_with("work/")
        || rel.starts_with(".fleet/")
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}

/// Walk `root` and collect allowlisted text files under the size caps.
pub fn collect_guest_files(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    walk(root, root, &mut out, &mut total);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>, total: &mut usize) {
    if out.len() >= MAX_GUEST_FILES || *total >= MAX_GUEST_TOTAL_CHARS {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    names.sort();
    for path in names {
        if out.len() >= MAX_GUEST_FILES || *total >= MAX_GUEST_TOTAL_CHARS {
            break;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let Some(rel) = safe_rel_path(&rel.to_string_lossy()) else {
            continue;
        };
        if path.is_dir() {
            if rel == "handoff" || rel == "work" || rel == ".fleet" || rel.starts_with("work/") {
                walk(root, &path, out, total);
            }
            continue;
        }
        if !should_pack_rel(&rel) {
            continue;
        }
        let Ok(meta) = path.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_GUEST_FILE_CHARS as u64 * 2 {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let text = truncate_chars(&raw, MAX_GUEST_FILE_CHARS);
        *total = total.saturating_add(text.len());
        out.insert(rel, text);
    }
}

pub fn write_guest_files(root: &Path, files: &BTreeMap<String, String>) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    for (rel, body) in files {
        let Some(rel) = safe_rel_path(rel) else {
            continue;
        };
        if !should_pack_rel(&rel) {
            continue;
        }
        let dest = root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, body)?;
    }
    Ok(())
}

pub fn ensure_fleet_tool(root: &Path) -> std::io::Result<()> {
    let dest = root.join("fleet-tool.sh");
    if dest.is_file() {
        return Ok(());
    }
    std::fs::write(&dest, FLEET_TOOL_SH)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
    }
    Ok(())
}

pub fn files_as_json_map(files: &BTreeMap<String, String>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in files {
        if k == "result.md" {
            continue;
        }
        out.insert(k.clone(), Value::String(v.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_parent_paths() {
        assert!(safe_rel_path("../etc/passwd").is_none());
        assert!(safe_rel_path("work/../../x").is_none());
        assert_eq!(safe_rel_path("/work/foo.rs").as_deref(), Some("work/foo.rs"));
    }

    #[test]
    fn collect_and_write_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "fleet-guest-files-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("work")).unwrap();
        fs::write(dir.join("TEAM.md"), "you are t1\n").unwrap();
        fs::write(dir.join("work/app.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("secret.bin"), [0u8, 1, 2]).unwrap();
        let files = collect_guest_files(&dir);
        assert!(files.get("TEAM.md").unwrap().contains("t1"));
        assert_eq!(files.get("work/app.rs").unwrap(), "fn main() {}\n");
        assert!(!files.contains_key("secret.bin"));

        let dest = dir.join("out");
        write_guest_files(&dest, &files).unwrap();
        assert_eq!(
            fs::read_to_string(dest.join("work/app.rs")).unwrap(),
            "fn main() {}\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
