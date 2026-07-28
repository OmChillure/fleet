//! Multi-field create/edit forms for projects, profiles, and tasks.

use crate::models::*;
use anyhow::{bail, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    Project { existing_id: Option<bool> },
    Profile { existing_id: Option<bool> },
    Task { existing_id: Option<bool> },
}

// Store entity id separately
#[derive(Debug, Clone)]
pub struct Form {
    pub kind: EntityKind,
    pub entity_id: Option<String>,
    pub title: String,
    pub fields: Vec<FormField>,
    pub field_idx: usize,
    /// When true, keystrokes edit the current field value.
    pub editing_value: bool,
    pub draft: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Project,
    Profile,
    Task,
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
    pub hint: &'static str,
}

impl Form {
    pub fn new_project() -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Self {
            kind: EntityKind::Project,
            entity_id: None,
            title: "New project".into(),
            fields: vec![
                FormField {
                    key: "name",
                    label: "Name",
                    value: String::new(),
                    hint: "display name",
                },
                FormField {
                    key: "path",
                    label: "Folder path",
                    value: cwd,
                    hint: "absolute path used as ACP cwd",
                },
                FormField {
                    key: "notes",
                    label: "Notes",
                    value: String::new(),
                    hint: "optional",
                },
            ],
            field_idx: 0,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn edit_project(p: &Project) -> Self {
        Self {
            kind: EntityKind::Project,
            entity_id: Some(p.id.clone()),
            title: format!("Edit project · {}", p.name),
            fields: vec![
                FormField {
                    key: "name",
                    label: "Name",
                    value: p.name.clone(),
                    hint: "display name",
                },
                FormField {
                    key: "path",
                    label: "Folder path",
                    value: p.path.clone(),
                    hint: "absolute path used as ACP cwd",
                },
                FormField {
                    key: "notes",
                    label: "Notes",
                    value: p.notes.clone(),
                    hint: "optional",
                },
            ],
            field_idx: 0,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn new_profile() -> Self {
        Self {
            kind: EntityKind::Profile,
            entity_id: None,
            title: "New harness profile".into(),
            fields: profile_fields(
                "",
                "custom",
                "",
                "",
                "",
                "",
                "",
                "[]",
                "[]",
                "{}",
                "true",
            ),
            field_idx: 0,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn edit_profile(p: &HarnessProfile) -> Self {
        let skills = serde_json::to_string_pretty(&p.skills).unwrap_or_else(|_| "[]".into());
        let mcp = serde_json::to_string_pretty(&p.mcp_servers).unwrap_or_else(|_| "[]".into());
        let env = serde_json::to_string_pretty(&p.env).unwrap_or_else(|_| "{}".into());
        let fields = profile_fields(
            &p.name,
            p.harness.as_str(),
            &p.command,
            &p.args.join(" "),
            p.model.as_deref().unwrap_or(""),
            p.thought_level.as_deref().unwrap_or(""),
            &p.system_prompt,
            &skills,
            &mcp,
            &env,
            if p.auto_approve { "true" } else { "false" },
        );
        // Focus Model field (index 2 after name, harness)
        let model_idx = fields.iter().position(|f| f.key == "model").unwrap_or(0);
        Self {
            kind: EntityKind::Profile,
            entity_id: Some(p.id.clone()),
            title: format!("Edit profile · {}", p.name),
            fields,
            field_idx: model_idx,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn new_task(project_id: &str, profile_id: &str, project_name: &str, profile_name: &str) -> Self {
        Self {
            kind: EntityKind::Task,
            entity_id: None,
            title: "New task".into(),
            fields: task_fields("", "", project_id, profile_id, project_name, profile_name),
            field_idx: 0,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn edit_task(
        t: &Task,
        project_id: &str,
        profile_id: &str,
        project_name: &str,
        profile_name: &str,
    ) -> Self {
        let fields = task_fields(
            &t.title,
            &t.prompt,
            project_id,
            profile_id,
            project_name,
            profile_name,
        );
        let profile_idx = fields
            .iter()
            .position(|f| f.key == "profile")
            .unwrap_or(0);
        Self {
            kind: EntityKind::Task,
            entity_id: Some(t.id.clone()),
            title: format!("Edit task · {}", t.title),
            fields,
            field_idx: profile_idx,
            editing_value: false,
            draft: String::new(),
        }
    }

    pub fn current_field(&self) -> &FormField {
        &self.fields[self.field_idx]
    }

    pub fn move_field(&mut self, delta: i32) {
        if self.fields.is_empty() {
            return;
        }
        let i = self.field_idx as i32 + delta;
        self.field_idx = if i < 0 {
            0
        } else if i as usize >= self.fields.len() {
            self.fields.len() - 1
        } else {
            i as usize
        };
    }

    pub fn begin_edit(&mut self) {
        self.draft = self.fields[self.field_idx].value.clone();
        self.editing_value = true;
    }

    pub fn commit_field_edit(&mut self) {
        if self.editing_value {
            self.fields[self.field_idx].value = self.draft.clone();
            self.editing_value = false;
            self.draft.clear();
        }
    }

    pub fn cancel_field_edit(&mut self) {
        self.editing_value = false;
        self.draft.clear();
    }

    pub fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }

    pub fn build_project(&self, existing: Option<&Project>) -> Result<Project> {
        let name = self.get("name").trim();
        let path = self.get("path").trim();
        if name.is_empty() {
            bail!("name is required");
        }
        if path.is_empty() {
            bail!("folder path is required");
        }
        let ts = now_rfc3339();
        Ok(Project {
            id: existing
                .map(|p| p.id.clone())
                .unwrap_or_else(new_id),
            name: name.to_string(),
            path: path.to_string(),
            notes: self.get("notes").to_string(),
            created_at: existing
                .map(|p| p.created_at.clone())
                .unwrap_or_else(|| ts.clone()),
            updated_at: ts,
        })
    }

    pub fn build_profile(&self, existing: Option<&HarnessProfile>) -> Result<HarnessProfile> {
        let name = self.get("name").trim();
        let command = self.get("command").trim();
        if name.is_empty() {
            bail!("name is required");
        }
        if command.is_empty() {
            bail!("command is required");
        }
        let harness = HarnessKind::parse(self.get("harness"));
        let args = parse_args(self.get("args"));
        let model = {
            let m = self.get("model").trim();
            if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            }
        };
        let thought_level = {
            let m = self.get("thought_level").trim();
            if m.is_empty() {
                None
            } else {
                Some(m.to_string())
            }
        };
        let skills: Vec<SkillRef> = parse_json_or_empty(self.get("skills_json"))?;
        let mcp_servers: Vec<McpServerConfig> = parse_json_or_empty(self.get("mcp_json"))?;
        let env: HashMap<String, String> = parse_json_or_empty(self.get("env_json"))?;
        let auto_approve = parse_bool(self.get("auto_approve"));
        let ts = now_rfc3339();
        Ok(HarnessProfile {
            id: existing
                .map(|p| p.id.clone())
                .unwrap_or_else(new_id),
            name: name.to_string(),
            harness,
            command: command.to_string(),
            args,
            model,
            thought_level,
            system_prompt: self.get("system_prompt").to_string(),
            skills,
            mcp_servers,
            env,
            auto_approve,
            // Preserve previously discovered ACP options unless wiped
            discovered: existing
                .map(|p| p.discovered.clone())
                .unwrap_or_default(),
            created_at: existing
                .map(|p| p.created_at.clone())
                .unwrap_or_else(|| ts.clone()),
            updated_at: ts,
        })
    }

    pub fn build_task(
        &self,
        existing: Option<&Task>,
        projects: &[Project],
        profiles: &[HarnessProfile],
    ) -> Result<Task> {
        let title = self.get("title").trim();
        let prompt = self.get("prompt").trim();
        if title.is_empty() {
            bail!("title is required");
        }
        if prompt.is_empty() {
            bail!("prompt is required");
        }

        let project_id = resolve_ref(self.get("project"), projects, |p| &p.id, |p| &p.name)?;
        let profile_id = resolve_ref(self.get("profile"), profiles, |p| &p.id, |p| &p.name)?;

        let ts = now_rfc3339();
        Ok(Task {
            id: existing
                .map(|t| t.id.clone())
                .unwrap_or_else(new_id),
            project_id,
            profile_id,
            title: title.to_string(),
            prompt: prompt.to_string(),
            status: existing.map(|t| t.status).unwrap_or(TaskStatus::Pending),
            created_at: existing
                .map(|t| t.created_at.clone())
                .unwrap_or_else(|| ts.clone()),
            updated_at: ts,
        })
    }
}

fn profile_fields(
    name: &str,
    harness: &str,
    command: &str,
    args: &str,
    model: &str,
    thought_level: &str,
    system_prompt: &str,
    skills: &str,
    mcp: &str,
    env: &str,
    auto_approve: &str,
) -> Vec<FormField> {
    vec![
        FormField {
            key: "name",
            label: "Name",
            value: name.into(),
            hint: "profile display name",
        },
        FormField {
            key: "harness",
            label: "Harness",
            value: harness.into(),
            hint: "codex | opencode | grok | custom",
        },
        FormField {
            key: "model",
            label: "Model",
            value: model.into(),
            hint: "Enter opens live ACP model list (do not type)",
        },
        FormField {
            key: "thought_level",
            label: "Thought level",
            value: thought_level.into(),
            hint: "Enter opens live ACP thought/effort list",
        },
        FormField {
            key: "command",
            label: "Command",
            value: command.into(),
            hint: "binary on PATH, e.g. grok / opencode / npx",
        },
        FormField {
            key: "args",
            label: "Args",
            value: args.into(),
            hint: "space-separated, e.g. agent stdio  or  acp",
        },
        FormField {
            key: "system_prompt",
            label: "System prompt",
            value: system_prompt.into(),
            hint: "prepended to every task prompt",
        },
        FormField {
            key: "skills_json",
            label: "Skills (JSON)",
            value: skills.into(),
            hint: r#"[{"path":"/abs/or/rel/SKILL.md","enabled":true}]"#,
        },
        FormField {
            key: "mcp_json",
            label: "MCP servers (JSON)",
            value: mcp.into(),
            hint: r#"[{"name":"x","command":"/bin/mcp","args":[],"env":{}}]"#,
        },
        FormField {
            key: "env_json",
            label: "Env (JSON)",
            value: env.into(),
            hint: r#"{"KEY":"value"}"#,
        },
        FormField {
            key: "auto_approve",
            label: "Auto-approve",
            value: auto_approve.into(),
            hint: "true | false — ACP permission requests",
        },
    ]
}

fn task_fields(
    title: &str,
    prompt: &str,
    project_id: &str,
    profile_id: &str,
    project_name: &str,
    profile_name: &str,
) -> Vec<FormField> {
    vec![
        FormField {
            key: "title",
            label: "Title",
            value: title.into(),
            hint: "short label",
        },
        FormField {
            key: "prompt",
            label: "Prompt",
            value: prompt.into(),
            hint: "full task text sent to the agent",
        },
        FormField {
            key: "project",
            label: "Project",
            value: if project_name.is_empty() {
                project_id.into()
            } else {
                format!("{project_name} ({project_id})")
            },
            hint: "Enter opens a list picker (do not type)",
        },
        FormField {
            key: "profile",
            label: "Profile/harness",
            value: if profile_name.is_empty() {
                profile_id.into()
            } else {
                format!("{profile_name} ({profile_id})")
            },
            hint: "Enter opens a list picker — pick Grok/Codex/OpenCode profile",
        },
    ]
}

fn parse_args(s: &str) -> Vec<String> {
    let s = s.trim();
    if s.is_empty() {
        return vec![];
    }
    // allow JSON array
    if s.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
            return v;
        }
    }
    s.split_whitespace().map(|p| p.to_string()).collect()
}

fn parse_json_or_empty<T: serde::de::DeserializeOwned>(s: &str) -> Result<T>
where
    T: Default,
{
    let s = s.trim();
    if s.is_empty() {
        return Ok(T::default());
    }
    Ok(serde_json::from_str(s)?)
}

fn parse_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn resolve_ref<T, FId, FName>(
    raw: &str,
    items: &[T],
    id_of: FId,
    name_of: FName,
) -> Result<String>
where
    FId: Fn(&T) -> &str,
    FName: Fn(&T) -> &str,
{
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("reference is required");
    }
    // "Name (uuid)" form
    if let Some(start) = raw.rfind('(') {
        if raw.ends_with(')') {
            let id = &raw[start + 1..raw.len() - 1];
            if items.iter().any(|i| id_of(i) == id) {
                return Ok(id.to_string());
            }
        }
    }
    if let Some(i) = items.iter().find(|i| id_of(i) == raw) {
        return Ok(id_of(i).to_string());
    }
    if let Some(i) = items.iter().find(|i| name_of(i) == raw) {
        return Ok(id_of(i).to_string());
    }
    // case-insensitive name
    if let Some(i) = items
        .iter()
        .find(|i| name_of(i).eq_ignore_ascii_case(raw))
    {
        return Ok(id_of(i).to_string());
    }
    bail!("unknown reference: {raw}");
}

/// Suggested starter values when user picks a known harness (not auto-seeded).
pub fn apply_harness_template(form: &mut Form, harness: &str) {
    let h = HarnessKind::parse(harness);
    let (command, args) = match h {
        HarnessKind::Opencode => ("opencode", "acp"),
        HarnessKind::Codex => ("npx", "-y @agentclientprotocol/codex-acp"),
        HarnessKind::Grok => ("grok", "agent stdio"),
        HarnessKind::Custom => ("", ""),
    };
    set_field(form, "harness", h.as_str());
    if !command.is_empty() {
        set_field(form, "command", command);
        set_field(form, "args", args);
    }
}

fn set_field(form: &mut Form, key: &str, value: &str) {
    if let Some(f) = form.fields.iter_mut().find(|f| f.key == key) {
        f.value = value.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_form() {
        let mut f = Form::new_project();
        set_field(&mut f, "name", "demo");
        set_field(&mut f, "path", "/tmp/x");
        let p = f.build_project(None).unwrap();
        assert_eq!(p.name, "demo");
        assert_eq!(p.path, "/tmp/x");
    }

    #[test]
    fn parse_profile_json_and_args() {
        let mut f = Form::new_profile();
        set_field(&mut f, "name", "Grok");
        set_field(&mut f, "harness", "grok");
        set_field(&mut f, "command", "grok");
        set_field(&mut f, "args", "agent stdio");
        set_field(&mut f, "model", "grok-4");
        set_field(
            &mut f,
            "skills_json",
            r#"[{"path":"/s/SKILL.md","enabled":true}]"#,
        );
        set_field(&mut f, "auto_approve", "false");
        let p = f.build_profile(None).unwrap();
        assert_eq!(p.harness, HarnessKind::Grok);
        assert_eq!(p.args, vec!["agent", "stdio"]);
        assert_eq!(p.model.as_deref(), Some("grok-4"));
        assert_eq!(p.skills.len(), 1);
        assert!(!p.auto_approve);
    }

    #[test]
    fn harness_template_grok() {
        let mut f = Form::new_profile();
        apply_harness_template(&mut f, "grok");
        assert_eq!(f.get("command"), "grok");
        assert_eq!(f.get("args"), "agent stdio");
    }
}
