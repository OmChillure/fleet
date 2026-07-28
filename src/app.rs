use crate::acp::{
    compose_user_message, discover_acp_config, run_acp_task, AcpRunRequest, LogPiece, StreamKind,
};
use crate::db::Db;
use crate::form::{apply_harness_template, EntityKind, Form};
use crate::model_select;
use crate::models::*;
use crate::models::DiscoveredConfig;
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Projects,
    Profiles,
    Tasks,
    Log,
}

impl Pane {
    pub fn next(self) -> Self {
        match self {
            Self::Projects => Self::Profiles,
            Self::Profiles => Self::Tasks,
            Self::Tasks => Self::Log,
            Self::Log => Self::Projects,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Projects => Self::Log,
            Self::Profiles => Self::Projects,
            Self::Tasks => Self::Profiles,
            Self::Log => Self::Tasks,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Form,
    ConfirmDelete,
    /// Pick or type a model for the selected profile.
    ModelPicker,
    /// Pick which profile (harness) a task uses, or pick project/profile inside a form.
    ProfilePicker,
}

/// Where a ref-picker selection should be written.
#[derive(Debug, Clone)]
pub enum RefPickerTarget {
    /// Update an existing saved task's profile_id.
    ExistingTask { task_id: String, task_title: String },
    /// Write into the open form field (profile or project).
    FormField { field_key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPickKind {
    Profile,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPickKind {
    Model,
    ThoughtLevel,
}

#[derive(Debug, Clone)]
pub struct ModelPicker {
    pub kind: ConfigPickKind,
    pub profile_id: String,
    pub profile_name: String,
    pub harness: HarnessKind,
    /// Options shown in the list (includes "" for default + ACP-fetched + custom).
    pub options: Vec<String>,
    pub selected: usize,
    /// When true, typing goes into custom_input instead of list nav.
    pub custom_mode: bool,
    pub custom_input: String,
    /// True while an ACP fetch is in flight for this picker.
    pub fetching: bool,
    /// If true, return to the profile form after pick (opened via form Enter).
    pub return_to_form: bool,
}

#[derive(Debug, Clone)]
pub struct ProfilePicker {
    pub kind: RefPickKind,
    pub target: RefPickerTarget,
    /// (id, display label)
    pub options: Vec<(String, String)>,
    pub selected: usize,
    pub title: String,
}

pub enum WorkerEvent {
    Log { run_id: String, piece: LogPiece },
    Finished {
        run_id: String,
        task_id: String,
        ok: bool,
        error: Option<String>,
    },
    Discovered {
        profile_id: String,
        result: Result<DiscoveredConfig, String>,
    },
}

pub struct App {
    pub db_path: PathBuf,
    pub projects: Vec<Project>,
    pub profiles: Vec<HarnessProfile>,
    pub tasks: Vec<TaskView>,
    pub selected_project: usize,
    pub selected_profile: usize,
    pub selected_task: usize,
    pub focus: Pane,
    pub input_mode: InputMode,
    pub form: Option<Form>,
    pub model_picker: Option<ModelPicker>,
    pub profile_picker: Option<ProfilePicker>,
    pub status: String,
    pub log_lines: Vec<String>,
    pub log_scroll: u16,
    /// Active streaming message kind so tokens append to one line.
    active_stream: Option<StreamKind>,
    pub running: bool,
    pub should_quit: bool,
    live_log: Arc<Mutex<Vec<LogPiece>>>,
    pub events_rx: mpsc::UnboundedReceiver<WorkerEvent>,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
    pending_delete: Option<Pane>,
}

impl App {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let db = Db::open(&db_path)?;
        // No mock seed data — only schema + migrations.
        crate::db::prepare_db(&db)?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut app = Self {
            db_path,
            projects: vec![],
            profiles: vec![],
            tasks: vec![],
            selected_project: 0,
            selected_profile: 0,
            selected_task: 0,
            focus: Pane::Projects,
            input_mode: InputMode::Normal,
            form: None,
            model_picker: None,
            profile_picker: None,
            status: "Agent Fleet — n new · e edit · m model · l thought · h harness · f fetch(in picker) · r run · q quit"
                .into(),
            log_lines: vec![
                "Empty workspace. Create a Project, then a Profile, then a Task.".into(),
                "Hints: grok → `grok agent stdio` · opencode → `opencode acp` · codex → `npx -y @agentclientprotocol/codex-acp`".into(),
                "In a new profile, set harness then press t to apply a command template.".into(),
            ],
            log_scroll: 0,
            active_stream: None,
            running: false,
            should_quit: false,
            live_log: Arc::new(Mutex::new(Vec::new())),
            events_rx,
            events_tx,
            pending_delete: None,
        };
        app.reload()?;
        Ok(app)
    }

    pub fn with_db<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Db) -> Result<T>,
    {
        let db = Db::open(&self.db_path)?;
        f(&db)
    }

    pub fn reload(&mut self) -> Result<()> {
        let db = Db::open(&self.db_path)?;
        self.projects = db.list_projects()?;
        self.profiles = db.list_profiles()?;
        self.tasks = db.list_task_views()?;
        if self.selected_project >= self.projects.len() && !self.projects.is_empty() {
            self.selected_project = self.projects.len() - 1;
        }
        if self.selected_profile >= self.profiles.len() && !self.profiles.is_empty() {
            self.selected_profile = self.profiles.len() - 1;
        }
        if self.selected_task >= self.tasks.len() && !self.tasks.is_empty() {
            self.selected_task = self.tasks.len() - 1;
        }
        Ok(())
    }

    pub fn push_log(&mut self, line: impl Into<String>) {
        self.end_stream();
        self.log_lines.push(line.into());
        self.trim_log();
    }

    fn end_stream(&mut self) {
        self.active_stream = None;
    }

    fn trim_log(&mut self) {
        if self.log_lines.len() > 2000 {
            let drain = self.log_lines.len() - 1500;
            self.log_lines.drain(0..drain);
        }
    }

    /// Apply a log piece: stream chunks append within one section; only switch
    /// headers when the *stream kind* changes (not on every newline).
    pub fn push_log_piece(&mut self, piece: LogPiece) {
        match piece {
            LogPiece::Stream { stream, text } => {
                if text.is_empty() {
                    return;
                }
                self.append_stream(stream, &text);
            }
            LogPiece::Line(line) => {
                self.push_log(line);
            }
        }
    }

    fn append_stream(&mut self, stream: StreamKind, text: &str) {
        if text.is_empty() {
            return;
        }

        let prefix = match stream {
            StreamKind::Thought => "💭 ",
            StreamKind::User => "👤 ",
            StreamKind::Agent => "",
        };

        // Header once per stream kind switch (agent ↔ thought), never per newline.
        if self.active_stream != Some(stream) {
            if !self.log_lines.is_empty() {
                self.log_lines.push(String::new());
            }
            let header = match stream {
                StreamKind::Thought => "── thought ──",
                StreamKind::User => "── user ──",
                StreamKind::Agent => "── assistant ──",
            };
            self.log_lines.push(header.into());
            self.active_stream = Some(stream);
        }

        // Split on \n but keep the same section: first segment continues the
        // current content line; further segments open new content rows.
        let ends_with_nl = text.ends_with('\n');
        let segments: Vec<&str> = text.split('\n').collect();
        for (i, seg) in segments.iter().enumerate() {
            let is_last = i + 1 == segments.len();
            // Skip the final empty segment created by a trailing '\n'
            if is_last && seg.is_empty() && ends_with_nl {
                self.log_lines.push(prefix.to_string());
                continue;
            }
            if i == 0 {
                match self.log_lines.last() {
                    Some(last) if last.starts_with("── ") || last.is_empty() => {
                        self.log_lines.push(format!("{prefix}{seg}"));
                    }
                    Some(_) => {
                        if let Some(last) = self.log_lines.last_mut() {
                            last.push_str(seg);
                        }
                    }
                    None => self.log_lines.push(format!("{prefix}{seg}")),
                }
            } else {
                self.log_lines.push(format!("{prefix}{seg}"));
            }
        }
        self.trim_log();
    }

    pub fn drain_worker_events(&mut self) {
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                WorkerEvent::Log { piece, .. } => self.push_log_piece(piece),
                WorkerEvent::Finished {
                    task_id,
                    ok,
                    error,
                    run_id: _,
                } => {
                    self.running = false;
                    self.end_stream();
                    if ok {
                        self.status = format!("Task {task_id:.8} finished OK");
                        self.push_log("✅ run finished");
                    } else {
                        let err = error.unwrap_or_else(|| "unknown error".into());
                        self.status = format!("Task failed: {err}");
                        self.push_log(format!("❌ {err}"));
                    }
                    let _ = self.reload();
                }
                WorkerEvent::Discovered { profile_id, result } => {
                    match result {
                        Ok(cfg) => {
                            let n_models = cfg.models.len();
                            let n_thought = cfg.thought_levels.len();
                            if let Ok(Some(mut p)) =
                                self.with_db(|db| db.get_profile(&profile_id))
                            {
                                p.discovered = cfg;
                                p.updated_at = now_rfc3339();
                                let _ = self.with_db(|db| db.upsert_profile(&p));
                            }
                            let _ = self.reload();
                            // refresh open picker if same profile
                            if let Some(picker) = self.model_picker.as_mut() {
                                if picker.profile_id == profile_id {
                                    let kind = picker.kind;
                                    let return_to_form = picker.return_to_form;
                                    let prev_sel = picker.selected;
                                    let profile = self
                                        .profiles
                                        .iter()
                                        .find(|p| p.id == profile_id)
                                        .cloned();
                                    if let Some(profile) = profile {
                                        let mut next = build_config_picker(kind, &profile);
                                        next.fetching = false;
                                        next.return_to_form = return_to_form;
                                        // Keep selection index in range after refresh
                                        if prev_sel < next.options.len() {
                                            next.selected = prev_sel;
                                        }
                                        *picker = next;
                                    } else {
                                        picker.fetching = false;
                                    }
                                }
                            }
                            self.status = format!(
                                "ACP: {n_models} models, {n_thought} thought levels — j/k · Enter"
                            );
                            self.push_log(format!(
                                "✓ discovered models={n_models} thought_levels={n_thought}"
                            ));
                        }
                        Err(e) => {
                            if let Some(picker) = self.model_picker.as_mut() {
                                if picker.profile_id == profile_id {
                                    picker.fetching = false;
                                }
                            }
                            self.status = format!("ACP discovery failed: {e}");
                            self.push_log(format!("! discovery failed: {e}"));
                        }
                    }
                }
            }
        }
        let pending: Vec<LogPiece> = self
            .live_log
            .lock()
            .map(|mut buf| buf.drain(..).collect())
            .unwrap_or_default();
        for piece in pending {
            self.push_log_piece(piece);
        }
    }

    pub fn inspector_text(&self) -> String {
        match self.focus {
            Pane::Projects => {
                if let Some(p) = self.projects.get(self.selected_project) {
                    format!(
                        "PROJECT\n  id: {}\n  name: {}\n  path: {}\n  notes: {}\n  updated: {}",
                        p.id, p.name, p.path, p.notes, p.updated_at
                    )
                } else {
                    "No project selected.\nPress n to create one.".into()
                }
            }
            Pane::Profiles => {
                if let Some(p) = self.profiles.get(self.selected_profile) {
                    let skills = serde_json::to_string_pretty(&p.skills).unwrap_or_default();
                    let mcp = serde_json::to_string_pretty(&p.mcp_servers).unwrap_or_default();
                    let env = serde_json::to_string_pretty(&p.env).unwrap_or_default();
                    format!(
                        "PROFILE\n  id: {}\n  name: {}\n  harness: {}\n  command: {} {}\n  model: {}  (m)\n  thought_level: {}  (l)\n  auto_approve: {}\n  discovered: {} models, {} thought levels  (f in picker)\n  system_prompt:\n{}\n  skills:\n{}\n  mcp:\n{}\n  env:\n{}",
                        p.id,
                        p.name,
                        p.harness,
                        p.command,
                        p.args.join(" "),
                        p.model.as_deref().unwrap_or("(default)"),
                        p.thought_level.as_deref().unwrap_or("(default)"),
                        p.auto_approve,
                        p.discovered.models.len(),
                        p.discovered.thought_levels.len(),
                        p.system_prompt,
                        skills,
                        mcp,
                        env
                    )
                } else {
                    "No profile selected.\nPress n to create one.\nIn the form: set harness then press t for a command template.\nPress m to set the model for a profile.".into()
                }
            }
            Pane::Tasks => {
                if let Some(t) = self.tasks.get(self.selected_task) {
                    let harness = self
                        .profiles
                        .iter()
                        .find(|p| p.id == t.task.profile_id)
                        .map(|p| p.harness.to_string())
                        .unwrap_or_else(|| "?".into());
                    format!(
                        "TASK\n  id: {}\n  title: {}\n  status: {}\n  project: {} ({})\n  profile: {} ({})\n  harness: {}  ← press h to change\n  prompt:\n{}",
                        t.task.id,
                        t.task.title,
                        t.task.status,
                        t.project_name,
                        t.task.project_id,
                        t.profile_name,
                        t.task.profile_id,
                        harness,
                        t.task.prompt
                    )
                } else {
                    "No task selected.\nNeed a project + profile first, then press n.\nPress h on a task to switch its profile/harness.".into()
                }
            }
            Pane::Log => "ACP run log (bottom panel). Press r on a task to run.".into(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match self.input_mode {
            InputMode::Form => return self.handle_form_key(key),
            InputMode::ConfirmDelete => return self.handle_confirm_key(key),
            InputMode::ModelPicker => return self.handle_model_picker_key(key),
            InputMode::ProfilePicker => return self.handle_profile_picker_key(key),
            InputMode::Normal => {}
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {}
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::BackTab => self.focus = self.focus.prev(),
            KeyCode::Char('1') => self.focus = Pane::Projects,
            KeyCode::Char('2') => self.focus = Pane::Profiles,
            KeyCode::Char('3') => self.focus = Pane::Tasks,
            KeyCode::Char('4') => self.focus = Pane::Log,
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Char('r') => self.run_selected_task()?,
            KeyCode::Char('n') => self.start_create()?,
            KeyCode::Char('e') | KeyCode::Enter => self.start_edit()?,
            KeyCode::Char('m') => self.start_config_picker(ConfigPickKind::Model)?,
            KeyCode::Char('l') => self.start_config_picker(ConfigPickKind::ThoughtLevel)?,
            KeyCode::Char('h') => self.start_profile_picker()?,
            KeyCode::Char('d') => {
                if matches!(self.focus, Pane::Log) {
                    self.status = "Select projects/profiles/tasks to delete".into();
                } else {
                    self.pending_delete = Some(self.focus);
                    self.input_mode = InputMode::ConfirmDelete;
                    self.status = "Delete selected item? y/N".into();
                }
            }
            KeyCode::Char('R') => {
                self.reload()?;
                self.status = "Reloaded from SQLite".into();
            }
            KeyCode::PageUp => self.log_scroll = self.log_scroll.saturating_add(5),
            KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_sub(5),
            KeyCode::Char('g') if self.focus == Pane::Log => self.log_scroll = 0,
            _ => {}
        }
        Ok(())
    }

    fn start_config_picker(&mut self, kind: ConfigPickKind) -> Result<()> {
        let profile = if self.focus == Pane::Tasks {
            self.tasks
                .get(self.selected_task)
                .and_then(|t| {
                    self.profiles
                        .iter()
                        .find(|p| p.id == t.task.profile_id)
                        .cloned()
                })
                .or_else(|| self.profiles.get(self.selected_profile).cloned())
        } else {
            self.profiles.get(self.selected_profile).cloned()
        };

        let Some(profile) = profile else {
            self.status = "Create a profile first, then press m (model) or l (thought level)".into();
            self.focus = Pane::Profiles;
            return Ok(());
        };

        self.open_config_picker_for_profile(kind, &profile, false)
    }

    fn open_config_picker_for_profile(
        &mut self,
        kind: ConfigPickKind,
        profile: &HarnessProfile,
        return_to_form: bool,
    ) -> Result<()> {
        // Always fetch live list from ACP (no hardcoded catalogs).
        let mut picker = build_config_picker(kind, profile);
        picker.fetching = true;
        picker.return_to_form = return_to_form;
        let profile_id = profile.id.clone();
        let profile_name = profile.name.clone();
        self.model_picker = Some(picker);
        self.input_mode = InputMode::ModelPicker;
        if !return_to_form {
            self.focus = Pane::Profiles;
        }
        let what = match kind {
            ConfigPickKind::Model => "Model",
            ConfigPickKind::ThoughtLevel => "Thought level",
        };
        self.status = format!("{what} for '{profile_name}' — fetching live list from ACP…");
        self.fetch_acp_options_for_profile(&profile_id)?;
        Ok(())
    }

    fn fetch_acp_options_for_profile(&mut self, profile_id: &str) -> Result<()> {
        let profile = self
            .with_db(|db| db.get_profile(profile_id))?
            .ok_or_else(|| anyhow::anyhow!("profile missing"))?;
        let cwd = self
            .projects
            .get(self.selected_project)
            .map(|p| PathBuf::from(&p.path))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        self.status = format!("Fetching models/thought levels from {}…", profile.name);
        self.push_log(format!(
            "▶ ACP discover {} {}",
            profile.command,
            profile.args.join(" ")
        ));
        let tx = self.events_tx.clone();
        let pid = profile_id.to_string();
        tokio::spawn(async move {
            let res = discover_acp_config(
                &profile.command,
                &profile.args,
                &profile.env,
                &cwd,
            )
            .await
            .map_err(|e| e.to_string());
            let _ = tx.send(WorkerEvent::Discovered {
                profile_id: pid,
                result: res,
            });
        });
        Ok(())
    }

    fn handle_model_picker_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(picker) = self.model_picker.as_mut() else {
            self.input_mode = InputMode::Normal;
            return Ok(());
        };

        if picker.custom_mode {
            match key.code {
                KeyCode::Esc => {
                    picker.custom_mode = false;
                    self.status = "Back to list".into();
                }
                KeyCode::Enter => {
                    let value = picker.custom_input.trim().to_string();
                    let id = picker.profile_id.clone();
                    let name = picker.profile_name.clone();
                    let kind = picker.kind;
                    let return_to_form = picker.return_to_form;
                    self.apply_config_to_profile(
                        &id,
                        kind,
                        if value.is_empty() { None } else { Some(value.clone()) },
                    )?;
                    self.finish_config_pick(
                        &id,
                        kind,
                        if value.is_empty() { None } else { Some(value) },
                        &name,
                        return_to_form,
                    )?;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.custom_input.push(c);
                }
                KeyCode::Backspace => {
                    picker.custom_input.pop();
                }
                _ => {}
            }
            return Ok(());
        }

        let len = picker.options.len();
        match key.code {
            KeyCode::Esc => {
                let return_to_form = picker.return_to_form;
                self.model_picker = None;
                if return_to_form && self.form.is_some() {
                    self.input_mode = InputMode::Form;
                    self.status = "Back to profile form — s to save".into();
                } else {
                    self.input_mode = InputMode::Normal;
                    self.status = "Cancelled".into();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if picker.selected + 1 < len {
                    picker.selected += 1;
                }
            }
            KeyCode::PageUp => {
                picker.selected = picker.selected.saturating_sub(10);
            }
            KeyCode::PageDown => {
                if len > 0 {
                    picker.selected = (picker.selected + 10).min(len - 1);
                }
            }
            KeyCode::Home | KeyCode::Char('g') => {
                picker.selected = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                if len > 0 {
                    picker.selected = len - 1;
                }
            }
            KeyCode::Char('c') => {
                picker.custom_mode = true;
                self.status = "Type value — Enter save, Esc back".into();
            }
            KeyCode::Char('f') => {
                let id = picker.profile_id.clone();
                picker.fetching = true;
                self.fetch_acp_options_for_profile(&id)?;
            }
            KeyCode::Enter => {
                let choice = picker
                    .options
                    .get(picker.selected)
                    .cloned()
                    .unwrap_or_default();
                if choice == "__custom__" {
                    picker.custom_mode = true;
                    self.status = "Type value — Enter save, Esc back".into();
                    return Ok(());
                }
                let id = picker.profile_id.clone();
                let name = picker.profile_name.clone();
                let kind = picker.kind;
                let return_to_form = picker.return_to_form;
                let value = if choice.is_empty() {
                    None
                } else {
                    Some(choice)
                };
                self.apply_config_to_profile(&id, kind, value.clone())?;
                self.finish_config_pick(&id, kind, value, &name, return_to_form)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_config_pick(
        &mut self,
        profile_id: &str,
        kind: ConfigPickKind,
        value: Option<String>,
        name: &str,
        return_to_form: bool,
    ) -> Result<()> {
        self.model_picker = None;
        // Sync open profile form fields if present
        if let Some(form) = self.form.as_mut() {
            if form.kind == EntityKind::Profile
                && form.entity_id.as_deref() == Some(profile_id)
            {
                let field = match kind {
                    ConfigPickKind::Model => "model",
                    ConfigPickKind::ThoughtLevel => "thought_level",
                };
                if let Some(f) = form.fields.iter_mut().find(|f| f.key == field) {
                    f.value = value.clone().unwrap_or_default();
                }
            }
        }
        let label = match kind {
            ConfigPickKind::Model => "model",
            ConfigPickKind::ThoughtLevel => "thought_level",
        };
        if return_to_form && self.form.is_some() {
            self.input_mode = InputMode::Form;
            self.status = format!(
                "Profile '{name}' {label} = {} — s to save form",
                value.as_deref().unwrap_or("(agent default)")
            );
        } else {
            self.input_mode = InputMode::Normal;
            self.status = format!(
                "Profile '{name}' {label} = {}",
                value.as_deref().unwrap_or("(agent default)")
            );
        }
        Ok(())
    }

    fn apply_config_to_profile(
        &mut self,
        profile_id: &str,
        kind: ConfigPickKind,
        value: Option<String>,
    ) -> Result<()> {
        let mut profile = self
            .with_db(|db| db.get_profile(profile_id))?
            .ok_or_else(|| anyhow::anyhow!("profile not found"))?;
        match kind {
            ConfigPickKind::Model => profile.model = value,
            ConfigPickKind::ThoughtLevel => profile.thought_level = value,
        }
        profile.updated_at = now_rfc3339();
        self.with_db(|db| db.upsert_profile(&profile))?;
        self.reload()?;
        if let Some(idx) = self.profiles.iter().position(|p| p.id == profile_id) {
            self.selected_profile = idx;
        }
        Ok(())
    }

    /// Change which profile (and thus harness/model/MCP) a task uses.
    fn start_profile_picker(&mut self) -> Result<()> {
        if self.profiles.is_empty() {
            self.status = "Create a profile first (pane Profiles, n)".into();
            self.focus = Pane::Profiles;
            return Ok(());
        }

        // If a task form is open on the profile field, fill that form.
        if self.input_mode == InputMode::Form {
            if let Some(form) = &self.form {
                if form.kind == EntityKind::Task {
                    return self.open_ref_picker_for_form("profile");
                }
            }
        }

        let task = self.tasks.get(self.selected_task).map(|t| t.task.clone());

        let Some(task) = task else {
            self.status = "Select a task, then press h — or create a task and pick profile on Enter".into();
            self.focus = Pane::Tasks;
            return Ok(());
        };

        self.open_profile_picker(
            RefPickerTarget::ExistingTask {
                task_id: task.id.clone(),
                task_title: task.title.clone(),
            },
            Some(&task.profile_id),
            format!("Profile / harness for '{}'", task.title),
        );
        Ok(())
    }

    fn open_profile_picker(
        &mut self,
        target: RefPickerTarget,
        current_id: Option<&str>,
        title: String,
    ) {
        let options: Vec<(String, String)> = self
            .profiles
            .iter()
            .map(|p| {
                let model = p
                    .model
                    .as_deref()
                    .map(|m| format!(" · model:{m}"))
                    .unwrap_or_default();
                (
                    p.id.clone(),
                    format!("{} [{}]{} · {}", p.name, p.harness, model, p.command),
                )
            })
            .collect();
        let selected = current_id
            .and_then(|cid| options.iter().position(|(id, _)| id == cid))
            .unwrap_or(0);
        self.profile_picker = Some(ProfilePicker {
            kind: RefPickKind::Profile,
            target,
            options,
            selected,
            title,
        });
        self.input_mode = InputMode::ProfilePicker;
        self.status = "j/k select profile · Enter confirm · Esc cancel".into();
    }

    fn open_project_picker(
        &mut self,
        target: RefPickerTarget,
        current_id: Option<&str>,
        title: String,
    ) {
        let options: Vec<(String, String)> = self
            .projects
            .iter()
            .map(|p| (p.id.clone(), format!("{} · {}", p.name, p.path)))
            .collect();
        let selected = current_id
            .and_then(|cid| options.iter().position(|(id, _)| id == cid))
            .unwrap_or(0);
        self.profile_picker = Some(ProfilePicker {
            kind: RefPickKind::Project,
            target,
            options,
            selected,
            title,
        });
        self.input_mode = InputMode::ProfilePicker;
        self.status = "j/k select project · Enter confirm · Esc cancel".into();
    }

    fn open_ref_picker_for_form(&mut self, field_key: &str) -> Result<()> {
        let form = self
            .form
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no form"))?;
        let current_raw = form.get(field_key).to_string();
        let current_id = extract_id_from_ref_display(&current_raw);

        match field_key {
            "profile" => {
                if self.profiles.is_empty() {
                    self.status = "No profiles — create one first".into();
                    return Ok(());
                }
                self.open_profile_picker(
                    RefPickerTarget::FormField {
                        field_key: "profile".into(),
                    },
                    current_id.as_deref(),
                    "Select profile / harness for this task".into(),
                );
            }
            "project" => {
                if self.projects.is_empty() {
                    self.status = "No projects — create one first".into();
                    return Ok(());
                }
                self.open_project_picker(
                    RefPickerTarget::FormField {
                        field_key: "project".into(),
                    },
                    current_id.as_deref(),
                    "Select project (folder) for this task".into(),
                );
            }
            _ => {
                self.status = format!("Field '{field_key}' is not a pickable reference");
            }
        }
        Ok(())
    }

    fn handle_profile_picker_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(picker) = self.profile_picker.as_mut() else {
            self.input_mode = InputMode::Normal;
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                let back_to_form = matches!(
                    &picker.target,
                    RefPickerTarget::FormField { .. }
                );
                self.profile_picker = None;
                if back_to_form && self.form.is_some() {
                    self.input_mode = InputMode::Form;
                    self.status = "Back to form — s to save".into();
                } else {
                    self.input_mode = InputMode::Normal;
                    self.status = "Pick cancelled".into();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if picker.selected + 1 < picker.options.len() {
                    picker.selected += 1;
                }
            }
            KeyCode::Enter => {
                let Some((id, label)) = picker.options.get(picker.selected).cloned() else {
                    return Ok(());
                };
                let target = picker.target.clone();
                let kind = picker.kind;
                self.profile_picker = None;

                match target {
                    RefPickerTarget::ExistingTask {
                        task_id,
                        task_title,
                    } => {
                        match kind {
                            RefPickKind::Profile => {
                                self.apply_profile_to_task(&task_id, &id)?;
                                self.input_mode = InputMode::Normal;
                                self.status = format!("Task '{task_title}' → {label}");
                            }
                            RefPickKind::Project => {
                                self.apply_project_to_task(&task_id, &id)?;
                                self.input_mode = InputMode::Normal;
                                self.status = format!("Task '{task_title}' project → {label}");
                            }
                        }
                    }
                    RefPickerTarget::FormField { field_key } => {
                        if let Some(form) = self.form.as_mut() {
                            // Store as "Name (id)" so form resolvers work
                            let display = match kind {
                                RefPickKind::Profile => {
                                    let p = self.profiles.iter().find(|p| p.id == id);
                                    match p {
                                        Some(p) => format!("{} ({})", p.name, p.id),
                                        None => format!("{label} ({id})"),
                                    }
                                }
                                RefPickKind::Project => {
                                    let p = self.projects.iter().find(|p| p.id == id);
                                    match p {
                                        Some(p) => format!("{} ({})", p.name, p.id),
                                        None => format!("{label} ({id})"),
                                    }
                                }
                            };
                            if let Some(f) = form.fields.iter_mut().find(|f| f.key == field_key) {
                                f.value = display;
                            }
                            // Move to next field for smoother create flow
                            form.move_field(1);
                        }
                        self.input_mode = InputMode::Form;
                        self.status = format!("Selected: {label} — s to save form");
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_project_to_task(&mut self, task_id: &str, project_id: &str) -> Result<()> {
        let mut task = self
            .with_db(|db| db.get_task(task_id))?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        let project = self
            .with_db(|db| db.get_project(project_id))?
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        task.project_id = project.id.clone();
        task.updated_at = now_rfc3339();
        self.with_db(|db| db.upsert_task(&task))?;
        self.reload()?;
        if let Some(idx) = self.tasks.iter().position(|t| t.task.id == task_id) {
            self.selected_task = idx;
        }
        Ok(())
    }

    fn apply_profile_to_task(&mut self, task_id: &str, profile_id: &str) -> Result<()> {
        let mut task = self
            .with_db(|db| db.get_task(task_id))?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        // validate profile exists
        let profile = self
            .with_db(|db| db.get_profile(profile_id))?
            .ok_or_else(|| anyhow::anyhow!("profile not found"))?;
        task.profile_id = profile.id.clone();
        task.updated_at = now_rfc3339();
        // reset status so a re-run is clearly pending under new harness
        if !matches!(task.status, TaskStatus::Running) {
            task.status = TaskStatus::Pending;
        }
        self.with_db(|db| db.upsert_task(&task))?;
        self.reload()?;
        if let Some(idx) = self.tasks.iter().position(|t| t.task.id == task_id) {
            self.selected_task = idx;
        }
        Ok(())
    }

    fn start_create(&mut self) -> Result<()> {
        let form = match self.focus {
            Pane::Projects | Pane::Log => Form::new_project(),
            Pane::Profiles => Form::new_profile(),
            Pane::Tasks => {
                if self.projects.is_empty() {
                    self.status = "Create a project first (focus Projects, press n)".into();
                    return Ok(());
                }
                if self.profiles.is_empty() {
                    self.status = "Create a profile first (focus Profiles, press n)".into();
                    return Ok(());
                }
                let project = &self.projects[self.selected_project.min(self.projects.len() - 1)];
                let profile = &self.profiles[self.selected_profile.min(self.profiles.len() - 1)];
                Form::new_task(&project.id, &profile.id, &project.name, &profile.name)
            }
        };
        self.status = format!(
            "{} — j/k fields · Enter (picker on Profile/Project) · s save · Esc",
            form.title
        );
        self.form = Some(form);
        self.input_mode = InputMode::Form;
        Ok(())
    }

    fn start_edit(&mut self) -> Result<()> {
        let form = match self.focus {
            Pane::Projects => {
                let Some(p) = self.projects.get(self.selected_project) else {
                    self.status = "No project to edit — press n to create".into();
                    return Ok(());
                };
                Form::edit_project(p)
            }
            Pane::Profiles => {
                let Some(p) = self.profiles.get(self.selected_profile) else {
                    self.status = "No profile to edit — press n to create".into();
                    return Ok(());
                };
                Form::edit_profile(p)
            }
            Pane::Tasks => {
                let Some(t) = self.tasks.get(self.selected_task) else {
                    self.status = "No task to edit — press n to create".into();
                    return Ok(());
                };
                Form::edit_task(
                    &t.task,
                    &t.task.project_id,
                    &t.task.profile_id,
                    &t.project_name,
                    &t.profile_name,
                )
            }
            Pane::Log => {
                self.status = "Switch to Projects/Profiles/Tasks to edit".into();
                return Ok(());
            }
        };
        self.status = format!(
            "{} — j/k fields · Enter on Profile = list picker · s save · Esc",
            form.title
        );
        self.form = Some(form);
        self.input_mode = InputMode::Form;
        Ok(())
    }

    fn handle_form_key(&mut self, key: KeyEvent) -> Result<()> {
        // Peek form state without holding a long &mut borrow (needed to open pickers).
        let (entity, field_key, editing, field_label) = {
            let Some(form) = self.form.as_ref() else {
                self.input_mode = InputMode::Normal;
                return Ok(());
            };
            (
                form.kind,
                form.current_field().key,
                form.editing_value,
                form.current_field().label,
            )
        };

        if editing {
            let form = self.form.as_mut().unwrap();
            match key.code {
                KeyCode::Esc => {
                    form.cancel_field_edit();
                    self.status = "Field edit cancelled".into();
                }
                KeyCode::Enter => {
                    form.commit_field_edit();
                    self.status = "Field updated (s to save form)".into();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.draft.push(c);
                }
                KeyCode::Backspace => {
                    form.draft.pop();
                }
                _ => {}
            }
            return Ok(());
        }

        // Task form: profile/project list pickers
        let pickable_task_ref =
            entity == EntityKind::Task && matches!(field_key, "profile" | "project");
        // Profile form: model / thought level list pickers (live ACP)
        let pickable_profile_config = entity == EntityKind::Profile
            && matches!(field_key, "model" | "thought_level");

        match key.code {
            KeyCode::Esc => {
                self.form = None;
                self.input_mode = InputMode::Normal;
                self.status = "Form cancelled".into();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(form) = self.form.as_mut() {
                    form.move_field(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(form) = self.form.as_mut() {
                    form.move_field(1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if pickable_task_ref => {
                return self.open_ref_picker_for_form(field_key);
            }
            KeyCode::Enter | KeyCode::Char(' ') if pickable_profile_config => {
                let kind = if field_key == "model" {
                    ConfigPickKind::Model
                } else {
                    ConfigPickKind::ThoughtLevel
                };
                // Prefer the profile being edited (form entity_id)
                let profile = self
                    .form
                    .as_ref()
                    .and_then(|f| f.entity_id.clone())
                    .and_then(|id| self.profiles.iter().find(|p| p.id == id).cloned())
                    .or_else(|| self.profiles.get(self.selected_profile).cloned());
                let Some(profile) = profile else {
                    self.status = "Save/create the profile first, then set model".into();
                    return Ok(());
                };
                return self.open_config_picker_for_profile(kind, &profile, true);
            }
            // From any field on a task form, h opens profile picker.
            KeyCode::Char('h') if entity == EntityKind::Task => {
                return self.open_ref_picker_for_form("profile");
            }
            // p on task form = project picker
            KeyCode::Char('p') if entity == EntityKind::Task => {
                return self.open_ref_picker_for_form("project");
            }
            // m/l while editing a profile form
            KeyCode::Char('m') if entity == EntityKind::Profile => {
                let profile = self
                    .form
                    .as_ref()
                    .and_then(|f| f.entity_id.clone())
                    .and_then(|id| self.profiles.iter().find(|p| p.id == id).cloned())
                    .or_else(|| self.profiles.get(self.selected_profile).cloned());
                if let Some(profile) = profile {
                    return self.open_config_picker_for_profile(
                        ConfigPickKind::Model,
                        &profile,
                        true,
                    );
                }
            }
            KeyCode::Char('l') if entity == EntityKind::Profile => {
                let profile = self
                    .form
                    .as_ref()
                    .and_then(|f| f.entity_id.clone())
                    .and_then(|id| self.profiles.iter().find(|p| p.id == id).cloned())
                    .or_else(|| self.profiles.get(self.selected_profile).cloned());
                if let Some(profile) = profile {
                    return self.open_config_picker_for_profile(
                        ConfigPickKind::ThoughtLevel,
                        &profile,
                        true,
                    );
                }
            }
            KeyCode::Enter => {
                if let Some(form) = self.form.as_mut() {
                    form.begin_edit();
                    self.status = format!(
                        "Editing {field_label} — type, Enter accept, Esc cancel field"
                    );
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_form()?;
            }
            KeyCode::Char('t') => {
                if entity == EntityKind::Profile {
                    if let Some(form) = self.form.as_mut() {
                        let harness = form.get("harness").to_string();
                        apply_harness_template(form, &harness);
                        self.status =
                            "Applied harness command template (edit fields, s to save)".into();
                    }
                }
            }
            KeyCode::Char('s') => {
                self.save_form()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn save_form(&mut self) -> Result<()> {
        let Some(mut form) = self.form.take() else {
            self.input_mode = InputMode::Normal;
            return Ok(());
        };
        if form.editing_value {
            form.commit_field_edit();
        }

        let outcome = match form.kind {
            EntityKind::Project => {
                let existing = form
                    .entity_id
                    .as_ref()
                    .and_then(|id| self.projects.iter().find(|p| &p.id == id).cloned());
                form.build_project(existing.as_ref()).and_then(|p| {
                    self.with_db(|db| db.upsert_project(&p))?;
                    Ok((EntityKind::Project, format!("Saved project '{}'", p.name)))
                })
            }
            EntityKind::Profile => {
                let existing = form
                    .entity_id
                    .as_ref()
                    .and_then(|id| self.profiles.iter().find(|p| &p.id == id).cloned());
                form.build_profile(existing.as_ref()).and_then(|p| {
                    self.with_db(|db| db.upsert_profile(&p))?;
                    Ok((EntityKind::Profile, format!("Saved profile '{}'", p.name)))
                })
            }
            EntityKind::Task => {
                let existing = form.entity_id.as_ref().and_then(|id| {
                    self.tasks
                        .iter()
                        .find(|t| &t.task.id == id)
                        .map(|t| t.task.clone())
                });
                form.build_task(existing.as_ref(), &self.projects, &self.profiles)
                    .and_then(|task| {
                        self.with_db(|db| db.upsert_task(&task))?;
                        Ok((EntityKind::Task, format!("Saved task '{}'", task.title)))
                    })
            }
        };

        match outcome {
            Ok((kind, msg)) => {
                self.input_mode = InputMode::Normal;
                self.reload()?;
                self.focus = match kind {
                    EntityKind::Project => Pane::Projects,
                    EntityKind::Profile => Pane::Profiles,
                    EntityKind::Task => Pane::Tasks,
                };
                self.status = msg;
            }
            Err(e) => {
                self.status = format!("Save failed: {e} — fix fields and press s");
                self.form = Some(form);
                self.input_mode = InputMode::Form;
            }
        }
        Ok(())
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_delete()?,
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.input_mode = InputMode::Normal;
                self.pending_delete = None;
                self.status = "Delete cancelled".into();
            }
            _ => {}
        }
        Ok(())
    }

    fn confirm_delete(&mut self) -> Result<()> {
        let pane = self.pending_delete.take();
        self.input_mode = InputMode::Normal;
        match pane {
            Some(Pane::Projects) => {
                if let Some(p) = self.projects.get(self.selected_project) {
                    let id = p.id.clone();
                    let name = p.name.clone();
                    self.with_db(|db| db.delete_project(&id))?;
                    self.status = format!("Deleted project {name}");
                    self.reload()?;
                }
            }
            Some(Pane::Profiles) => {
                if let Some(p) = self.profiles.get(self.selected_profile) {
                    let id = p.id.clone();
                    let name = p.name.clone();
                    self.with_db(|db| db.delete_profile(&id))?;
                    self.status = format!("Deleted profile {name}");
                    self.reload()?;
                }
            }
            Some(Pane::Tasks) => {
                if let Some(t) = self.tasks.get(self.selected_task) {
                    let id = t.task.id.clone();
                    let title = t.task.title.clone();
                    self.with_db(|db| db.delete_task(&id))?;
                    self.status = format!("Deleted task {title}");
                    self.reload()?;
                }
            }
            _ => {
                self.status = "Nothing to delete".into();
            }
        }
        Ok(())
    }

    fn move_sel(&mut self, delta: i32) {
        match self.focus {
            Pane::Projects => {
                self.selected_project = sat_add(self.selected_project, delta, self.projects.len())
            }
            Pane::Profiles => {
                self.selected_profile = sat_add(self.selected_profile, delta, self.profiles.len())
            }
            Pane::Tasks => {
                self.selected_task = sat_add(self.selected_task, delta, self.tasks.len())
            }
            Pane::Log => {
                if delta < 0 {
                    self.log_scroll = self.log_scroll.saturating_add((-delta) as u16);
                } else {
                    self.log_scroll = self.log_scroll.saturating_sub(delta as u16);
                }
            }
        }
    }

    pub fn run_selected_task(&mut self) -> Result<()> {
        if self.running {
            self.status = "Already running a task".into();
            return Ok(());
        }
        let Some(view) = self.tasks.get(self.selected_task).cloned() else {
            self.status = "No task selected".into();
            return Ok(());
        };
        let profile = self
            .with_db(|db| db.get_profile(&view.task.profile_id))?
            .ok_or_else(|| anyhow::anyhow!("profile missing"))?;
        let project = self
            .with_db(|db| db.get_project(&view.task.project_id))?
            .ok_or_else(|| anyhow::anyhow!("project missing"))?;

        let run_id = new_id();
        let ts = now_rfc3339();
        let run = TaskRun {
            id: run_id.clone(),
            task_id: view.task.id.clone(),
            status: TaskStatus::Running,
            session_id: None,
            log: String::new(),
            error: None,
            started_at: ts,
            finished_at: None,
        };
        self.with_db(|db| {
            db.insert_run(&run)?;
            db.set_task_status(&view.task.id, TaskStatus::Running)
        })?;

        self.running = true;
        self.log_lines.clear();
        self.log_scroll = 0;
        self.active_stream = None;
        self.focus = Pane::Log;
        self.status = format!("Running '{}' via {}…", view.task.title, profile.name);
        self.push_log(format!(
            "═══ Run {} · {} · {} · model={} · thought={} ═══",
            view.task.title,
            profile.name,
            project.path,
            profile.model.as_deref().unwrap_or("(default)"),
            profile.thought_level.as_deref().unwrap_or("(default)")
        ));

        let tx = self.events_tx.clone();
        let db_path = self.db_path.clone();
        let task_id = view.task.id.clone();
        let run_id_c = run_id.clone();
        // Include title + prompt — title often holds the real ask.
        let user_prompt = compose_user_message(&view.task.title, &view.task.prompt);
        self.push_log(format!("→ user message:\n{user_prompt}"));

        tokio::spawn(async move {
            let log_tx = tx.clone();
            let rid = run_id_c.clone();
            let log_fn: crate::acp::LogFn = Arc::new(move |piece: LogPiece| {
                let _ = log_tx.send(WorkerEvent::Log {
                    run_id: rid.clone(),
                    piece,
                });
            });

            let spawn_args = model_select::inject_model_args(
                profile.harness,
                &profile.args,
                profile.model.as_deref(),
            );
            let req = AcpRunRequest {
                command: profile.command.clone(),
                args: spawn_args,
                env: profile.env.clone(),
                cwd: PathBuf::from(&project.path),
                model: profile.model.clone(),
                thought_level: profile.thought_level.clone(),
                system_prompt: profile.system_prompt.clone(),
                user_prompt,
                skills: profile.skills.clone(),
                mcp_servers: profile.mcp_servers.clone(),
                auto_approve: profile.auto_approve,
                log: log_fn,
            };

            let result = run_acp_task(req).await;

            let db = Db::open(&db_path);
            match (result, db) {
                (Ok(res), Ok(db)) => {
                    let run = TaskRun {
                        id: run_id_c.clone(),
                        task_id: task_id.clone(),
                        status: TaskStatus::Done,
                        session_id: res.session_id,
                        log: res.transcript,
                        error: None,
                        started_at: now_rfc3339(),
                        finished_at: Some(now_rfc3339()),
                    };
                    let _ = db.update_run(&run);
                    let _ = db.set_task_status(&task_id, TaskStatus::Done);
                    let _ = tx.send(WorkerEvent::Finished {
                        run_id: run_id_c,
                        task_id,
                        ok: true,
                        error: None,
                    });
                }
                (Err(e), Ok(db)) => {
                    let err = e.to_string();
                    let run = TaskRun {
                        id: run_id_c.clone(),
                        task_id: task_id.clone(),
                        status: TaskStatus::Failed,
                        session_id: None,
                        log: err.clone(),
                        error: Some(err.clone()),
                        started_at: now_rfc3339(),
                        finished_at: Some(now_rfc3339()),
                    };
                    let _ = db.update_run(&run);
                    let _ = db.set_task_status(&task_id, TaskStatus::Failed);
                    let _ = tx.send(WorkerEvent::Finished {
                        run_id: run_id_c,
                        task_id,
                        ok: false,
                        error: Some(err),
                    });
                }
                (Ok(_), Err(e)) | (Err(_), Err(e)) => {
                    let _ = tx.send(WorkerEvent::Finished {
                        run_id: run_id_c,
                        task_id,
                        ok: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        });

        self.reload()?;
        Ok(())
    }
}

fn sat_add(idx: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let i = idx as i32 + delta;
    if i < 0 {
        0
    } else if i as usize >= len {
        len - 1
    } else {
        i as usize
    }
}

/// Parse `Name (uuid)` form field display back to id, or return the raw string if it is already an id.
fn extract_id_from_ref_display(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(start) = raw.rfind('(') {
        if raw.ends_with(')') {
            let id = &raw[start + 1..raw.len() - 1];
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    Some(raw.to_string())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::{LogPiece, StreamKind};

    #[test]
    fn stream_chunks_merge_into_one_line() {
        // Build a minimal app without opening real TUI: exercise push_log_piece only.
        let dir = std::env::temp_dir().join(format!("af-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let mut app = App::new(db).unwrap();
        app.log_lines.clear();
        app.active_stream = None;

        app.push_log_piece(LogPiece::Stream {
            stream: StreamKind::Agent,
            text: "Hello".into(),
        });
        app.push_log_piece(LogPiece::Stream {
            stream: StreamKind::Agent,
            text: " world".into(),
        });
        app.push_log_piece(LogPiece::Stream {
            stream: StreamKind::Agent,
            text: "!".into(),
        });
        // header + merged stream line
        assert!(app.log_lines.iter().any(|l| l == "Hello world!"));
        let merged: String = app
            .log_lines
            .iter()
            .filter(|l| l.as_str() == "Hello world!")
            .cloned()
            .collect();
        assert_eq!(merged, "Hello world!");

        app.push_log_piece(LogPiece::Line("🔧 tool".into()));
        app.push_log_piece(LogPiece::Stream {
            stream: StreamKind::Agent,
            text: "next".into(),
        });
        assert!(app.log_lines.iter().any(|l| l == "🔧 tool"));
        assert!(app.log_lines.iter().any(|l| l == "next"));

        let _ = std::fs::remove_dir_all(dir);
    }
}


/// Build picker options from **ACP-discovered** lists only (no hardcoded catalogs).
fn build_config_picker(kind: ConfigPickKind, profile: &HarnessProfile) -> ModelPicker {
    let mut options: Vec<String> = vec![String::new()]; // agent default
    match kind {
        ConfigPickKind::Model => {
            for m in &profile.discovered.models {
                if !options.iter().any(|o| o == &m.value) {
                    options.push(m.value.clone());
                }
            }
            // Keep currently saved value visible even if agent no longer lists it
            if let Some(cur) = &profile.model {
                if !options.iter().any(|o| o == cur) {
                    options.push(cur.clone());
                }
            }
        }
        ConfigPickKind::ThoughtLevel => {
            for m in &profile.discovered.thought_levels {
                if !options.iter().any(|o| o == &m.value) {
                    options.push(m.value.clone());
                }
            }
            if let Some(cur) = &profile.thought_level {
                if !options.iter().any(|o| o == cur) {
                    options.push(cur.clone());
                }
            }
        }
    }
    options.push("__custom__".into());

    let current = match kind {
        ConfigPickKind::Model => profile.model.clone(),
        ConfigPickKind::ThoughtLevel => profile.thought_level.clone(),
    };
    let selected = current
        .as_ref()
        .and_then(|m| options.iter().position(|o| o == m))
        .unwrap_or(0);

    ModelPicker {
        kind,
        profile_id: profile.id.clone(),
        profile_name: profile.name.clone(),
        harness: profile.harness,
        options,
        selected,
        custom_mode: false,
        custom_input: current.unwrap_or_default(),
        fetching: false,
        return_to_form: false,
    }
}
