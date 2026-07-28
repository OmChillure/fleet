use crate::models::*;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                notes TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS harness_profiles (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                harness TEXT NOT NULL,
                command TEXT NOT NULL,
                args_json TEXT NOT NULL DEFAULT '[]',
                model TEXT,
                system_prompt TEXT NOT NULL DEFAULT '',
                skills_json TEXT NOT NULL DEFAULT '[]',
                mcp_json TEXT NOT NULL DEFAULT '[]',
                env_json TEXT NOT NULL DEFAULT '{}',
                auto_approve INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                profile_id TEXT NOT NULL REFERENCES harness_profiles(id),
                title TEXT NOT NULL,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                session_id TEXT,
                log TEXT NOT NULL DEFAULT '',
                error TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT
            );

            CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_runs_task ON task_runs(task_id);
            "#,
        )?;
        self.ensure_column("harness_profiles", "thought_level", "TEXT")?;
        self.ensure_column(
            "harness_profiles",
            "discovered_json",
            "TEXT NOT NULL DEFAULT '{}'",
        )?;
        Ok(())
    }

    fn ensure_column(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !cols.iter().any(|c| c == column) {
            self.conn
                .execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
        }
        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM app_meta WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ── Projects ──────────────────────────────────────────────

    pub fn upsert_project(&self, p: &Project) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects(id, name, path, notes, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, path=excluded.path, notes=excluded.notes,
               updated_at=excluded.updated_at",
            params![p.id, p.name, p.path, p.notes, p.created_at, p.updated_at],
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, path, notes, created_at, updated_at FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                notes: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, path, notes, created_at, updated_at FROM projects WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                notes: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Profiles ──────────────────────────────────────────────

    pub fn upsert_profile(&self, p: &HarnessProfile) -> Result<()> {
        self.conn.execute(
            "INSERT INTO harness_profiles(
                id, name, harness, command, args_json, model, thought_level, system_prompt,
                skills_json, mcp_json, env_json, auto_approve, discovered_json, created_at, updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, harness=excluded.harness, command=excluded.command,
               args_json=excluded.args_json, model=excluded.model, thought_level=excluded.thought_level,
               system_prompt=excluded.system_prompt, skills_json=excluded.skills_json,
               mcp_json=excluded.mcp_json, env_json=excluded.env_json,
               auto_approve=excluded.auto_approve, discovered_json=excluded.discovered_json,
               updated_at=excluded.updated_at",
            params![
                p.id,
                p.name,
                p.harness.as_str(),
                p.command,
                serde_json::to_string(&p.args)?,
                p.model,
                p.thought_level,
                p.system_prompt,
                serde_json::to_string(&p.skills)?,
                serde_json::to_string(&p.mcp_servers)?,
                serde_json::to_string(&p.env)?,
                if p.auto_approve { 1 } else { 0 },
                serde_json::to_string(&p.discovered)?,
                p.created_at,
                p.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_profiles(&self) -> Result<Vec<HarnessProfile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, harness, command, args_json, model, thought_level, system_prompt,
                    skills_json, mcp_json, env_json, auto_approve, discovered_json, created_at, updated_at
             FROM harness_profiles ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| self.row_to_profile(row))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_profile(&self, id: &str) -> Result<Option<HarnessProfile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, harness, command, args_json, model, thought_level, system_prompt,
                    skills_json, mcp_json, env_json, auto_approve, discovered_json, created_at, updated_at
             FROM harness_profiles WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(self.row_to_profile(row)?))
        } else {
            Ok(None)
        }
    }

    fn row_to_profile(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<HarnessProfile> {
        let harness: String = row.get(2)?;
        let args_json: String = row.get(4)?;
        let skills_json: String = row.get(8)?;
        let mcp_json: String = row.get(9)?;
        let env_json: String = row.get(10)?;
        let auto: i64 = row.get(11)?;
        let discovered_json: String = row.get(12).unwrap_or_else(|_| "{}".into());
        Ok(HarnessProfile {
            id: row.get(0)?,
            name: row.get(1)?,
            harness: HarnessKind::parse(&harness),
            command: row.get(3)?,
            args: serde_json::from_str(&args_json).unwrap_or_default(),
            model: row.get(5)?,
            thought_level: row.get(6)?,
            system_prompt: row.get(7)?,
            skills: serde_json::from_str(&skills_json).unwrap_or_default(),
            mcp_servers: serde_json::from_str(&mcp_json).unwrap_or_default(),
            env: serde_json::from_str(&env_json).unwrap_or_default(),
            auto_approve: auto != 0,
            discovered: serde_json::from_str(&discovered_json).unwrap_or_default(),
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM harness_profiles WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Tasks ─────────────────────────────────────────────────

    pub fn upsert_task(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks(id, project_id, profile_id, title, prompt, status, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
               project_id=excluded.project_id, profile_id=excluded.profile_id,
               title=excluded.title, prompt=excluded.prompt, status=excluded.status,
               updated_at=excluded.updated_at",
            params![
                t.id,
                t.project_id,
                t.profile_id,
                t.title,
                t.prompt,
                t.status.as_str(),
                t.created_at,
                t.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, profile_id, title, prompt, status, created_at, updated_at
             FROM tasks ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get(5)?;
            Ok(Task {
                id: row.get(0)?,
                project_id: row.get(1)?,
                profile_id: row.get(2)?,
                title: row.get(3)?,
                prompt: row.get(4)?,
                status: TaskStatus::parse(&status),
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_task_views(&self) -> Result<Vec<TaskView>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.project_id, t.profile_id, t.title, t.prompt, t.status,
                    t.created_at, t.updated_at,
                    p.name, p.path, h.name, h.harness
             FROM tasks t
             JOIN projects p ON p.id = t.project_id
             JOIN harness_profiles h ON h.id = t.profile_id
             ORDER BY t.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get(5)?;
            let harness: String = row.get(11)?;
            Ok(TaskView {
                task: Task {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    profile_id: row.get(2)?,
                    title: row.get(3)?,
                    prompt: row.get(4)?,
                    status: TaskStatus::parse(&status),
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                },
                project_name: row.get(8)?,
                project_path: row.get(9)?,
                profile_name: row.get(10)?,
                harness: HarnessKind::parse(&harness),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, profile_id, title, prompt, status, created_at, updated_at
             FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let status: String = row.get(5)?;
            Ok(Some(Task {
                id: row.get(0)?,
                project_id: row.get(1)?,
                profile_id: row.get(2)?,
                title: row.get(3)?,
                prompt: row.get(4)?,
                status: TaskStatus::parse(&status),
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn set_task_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Runs ──────────────────────────────────────────────────

    pub fn insert_run(&self, r: &TaskRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_runs(id, task_id, status, session_id, log, error, started_at, finished_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                r.id,
                r.task_id,
                r.status.as_str(),
                r.session_id,
                r.log,
                r.error,
                r.started_at,
                r.finished_at,
            ],
        )?;
        Ok(())
    }

    pub fn update_run(&self, r: &TaskRun) -> Result<()> {
        self.conn.execute(
            "UPDATE task_runs SET status=?1, session_id=?2, log=?3, error=?4, finished_at=?5
             WHERE id=?6",
            params![
                r.status.as_str(),
                r.session_id,
                r.log,
                r.error,
                r.finished_at,
                r.id,
            ],
        )?;
        Ok(())
    }

    pub fn append_run_log(&self, run_id: &str, chunk: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE task_runs SET log = log || ?1 WHERE id = ?2",
            params![chunk, run_id],
        )?;
        Ok(())
    }

    pub fn latest_run_for_task(&self, task_id: &str) -> Result<Option<TaskRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, status, session_id, log, error, started_at, finished_at
             FROM task_runs WHERE task_id = ?1 ORDER BY started_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![task_id])?;
        if let Some(row) = rows.next()? {
            let status: String = row.get(2)?;
            Ok(Some(TaskRun {
                id: row.get(0)?,
                task_id: row.get(1)?,
                status: TaskStatus::parse(&status),
                session_id: row.get(3)?,
                log: row.get(4)?,
                error: row.get(5)?,
                started_at: row.get(6)?,
                finished_at: row.get(7)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn count_profiles(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM harness_profiles", [], |r| r.get(0))?)
    }
}

/// Resolve which binary to use for a harness on this machine.
pub fn resolve_command(candidates: &[&str]) -> Option<String> {
    for c in candidates {
        if which(c).is_some() {
            return Some(c.to_string());
        }
    }
    None
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(cmd);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}


/// Repair known-bad seeded harness commands (e.g. bare `grok` without ACP args).
pub fn migrate_harness_profiles(db: &Db) -> Result<()> {
    for p in db.list_profiles()? {
        if p.harness != HarnessKind::Grok {
            continue;
        }
        let has_stdio = p.args.iter().any(|a| a == "stdio");
        let looks_legacy = p.args.is_empty() || !has_stdio;
        if !looks_legacy {
            continue;
        }
        let mut fixed = p;
        fixed.command = resolve_command(&["grok"]).unwrap_or_else(|| "grok".into());
        fixed.args = vec!["agent".into(), "stdio".into()];
        if fixed.name == "Grok Build" {
            fixed.name = "Grok Build (ACP)".into();
        }
        fixed.updated_at = now_rfc3339();
        db.upsert_profile(&fixed)?;
    }
    Ok(())
}

/// Open-time setup: migrations only. Does not insert mock/sample rows.
pub fn prepare_db(db: &Db) -> Result<()> {
    migrate_harness_profiles(db)?;
    Ok(())
}

/// Test helper: insert starter harness profiles (not used by the app binary).
#[cfg(test)]
pub fn seed_test_profiles(db: &Db) -> Result<()> {
    let ts = now_rfc3339();
    for p in test_default_profiles(&ts) {
        db.upsert_profile(&p)?;
    }
    Ok(())
}

#[cfg(test)]
fn test_default_profiles(ts: &str) -> Vec<HarnessProfile> {
    vec![
        HarnessProfile {
            id: new_id(),
            name: "OpenCode (ACP)".into(),
            harness: HarnessKind::Opencode,
            command: resolve_command(&["opencode"]).unwrap_or_else(|| "opencode".into()),
            args: vec!["acp".into()],
            model: None,
            thought_level: None,
            discovered: Default::default(),
            system_prompt: String::new(),
            skills: vec![],
            mcp_servers: vec![],
            env: HashMap::new(),
            auto_approve: true,
            created_at: ts.into(),
            updated_at: ts.into(),
        },
        HarnessProfile {
            id: new_id(),
            name: "Codex (ACP)".into(),
            harness: HarnessKind::Codex,
            command: "npx".into(),
            args: vec!["-y".into(), "@agentclientprotocol/codex-acp".into()],
            model: None,
            thought_level: None,
            discovered: Default::default(),
            system_prompt: String::new(),
            skills: vec![],
            mcp_servers: vec![],
            env: HashMap::new(),
            auto_approve: true,
            created_at: ts.into(),
            updated_at: ts.into(),
        },
        HarnessProfile {
            id: new_id(),
            name: "Grok Build (ACP)".into(),
            harness: HarnessKind::Grok,
            command: resolve_command(&["grok"]).unwrap_or_else(|| "grok".into()),
            args: vec!["agent".into(), "stdio".into()],
            model: None,
            thought_level: None,
            discovered: Default::default(),
            system_prompt: String::new(),
            skills: vec![],
            mcp_servers: vec![],
            env: HashMap::new(),
            auto_approve: true,
            created_at: ts.into(),
            updated_at: ts.into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> (PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!("agent-fleet-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let db = Db::open(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn prepare_db_is_empty_and_profiles_helper_works() {
        let (dir, db) = temp_db();
        prepare_db(&db).unwrap();
        assert_eq!(db.count_profiles().unwrap(), 0);
        assert!(db.list_projects().unwrap().is_empty());
        assert!(db.list_tasks().unwrap().is_empty());

        seed_test_profiles(&db).unwrap();
        assert_eq!(db.count_profiles().unwrap(), 3);
        let grok = db
            .list_profiles()
            .unwrap()
            .into_iter()
            .find(|p| p.harness == HarnessKind::Grok)
            .unwrap();
        assert_eq!(grok.args, vec!["agent", "stdio"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn project_crud() {
        let (dir, db) = temp_db();
        let ts = now_rfc3339();
        let p = Project {
            id: new_id(),
            name: "demo".into(),
            path: "/tmp/demo".into(),
            notes: "n".into(),
            created_at: ts.clone(),
            updated_at: ts.clone(),
        };
        db.upsert_project(&p).unwrap();
        let got = db.get_project(&p.id).unwrap().unwrap();
        assert_eq!(got.name, "demo");
        assert_eq!(got.path, "/tmp/demo");

        let mut p2 = p.clone();
        p2.name = "demo2".into();
        p2.updated_at = now_rfc3339();
        db.upsert_project(&p2).unwrap();
        assert_eq!(db.get_project(&p.id).unwrap().unwrap().name, "demo2");

        db.delete_project(&p.id).unwrap();
        assert!(db.get_project(&p.id).unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn profile_roundtrip_json_fields() {
        let (dir, db) = temp_db();
        let ts = now_rfc3339();
        let mut env = HashMap::new();
        env.insert("FOO".into(), "bar".into());
        let profile = HarnessProfile {
            id: new_id(),
            name: "Custom".into(),
            harness: HarnessKind::Custom,
            command: "echo".into(),
            args: vec!["acp".into()],
            model: Some("m1".into()),
            thought_level: None,
            discovered: Default::default(),
            system_prompt: "be nice".into(),
            skills: vec![SkillRef {
                path: "/skills/x".into(),
                enabled: true,
            }],
            mcp_servers: vec![McpServerConfig {
                name: "s".into(),
                command: Some("mcp".into()),
                args: vec![],
                env: HashMap::new(),
                r#type: None,
                url: None,
                headers: HashMap::new(),
            }],
            env,
            auto_approve: false,
            created_at: ts.clone(),
            updated_at: ts,
        };
        db.upsert_profile(&profile).unwrap();
        let got = db.get_profile(&profile.id).unwrap().unwrap();
        assert_eq!(got.command, "echo");
        assert_eq!(got.args, vec!["acp"]);
        assert_eq!(got.model.as_deref(), Some("m1"));
        assert_eq!(got.skills.len(), 1);
        assert_eq!(got.mcp_servers[0].name, "s");
        assert_eq!(got.env.get("FOO").map(String::as_str), Some("bar"));
        assert!(!got.auto_approve);
        assert_eq!(db.list_profiles().unwrap().len(), 1);

        db.delete_profile(&profile.id).unwrap();
        assert!(db.get_profile(&profile.id).unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn task_and_run_lifecycle() {
        let (dir, db) = temp_db();
        let ts = now_rfc3339();
        let project = Project {
            id: new_id(),
            name: "p".into(),
            path: "/tmp/p".into(),
            notes: String::new(),
            created_at: ts.clone(),
            updated_at: ts.clone(),
        };
        let profile = HarnessProfile {
            id: new_id(),
            name: "h".into(),
            harness: HarnessKind::Opencode,
            command: "opencode".into(),
            args: vec!["acp".into()],
            model: None,
            thought_level: None,
            discovered: Default::default(),
            system_prompt: String::new(),
            skills: vec![],
            mcp_servers: vec![],
            env: HashMap::new(),
            auto_approve: true,
            created_at: ts.clone(),
            updated_at: ts.clone(),
        };
        db.upsert_project(&project).unwrap();
        db.upsert_profile(&profile).unwrap();

        let task = Task {
            id: new_id(),
            project_id: project.id.clone(),
            profile_id: profile.id.clone(),
            title: "t1".into(),
            prompt: "hello".into(),
            status: TaskStatus::Pending,
            created_at: ts.clone(),
            updated_at: ts.clone(),
        };
        db.upsert_task(&task).unwrap();
        assert_eq!(db.get_task(&task.id).unwrap().unwrap().title, "t1");

        let views = db.list_task_views().unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].project_name, "p");
        assert_eq!(views[0].profile_name, "h");
        assert_eq!(views[0].harness, HarnessKind::Opencode);

        let run = TaskRun {
            id: new_id(),
            task_id: task.id.clone(),
            status: TaskStatus::Running,
            session_id: None,
            log: "start\n".into(),
            error: None,
            started_at: ts.clone(),
            finished_at: None,
        };
        db.insert_run(&run).unwrap();
        db.append_run_log(&run.id, "more\n").unwrap();
        let latest = db.latest_run_for_task(&task.id).unwrap().unwrap();
        assert!(latest.log.contains("start"));
        assert!(latest.log.contains("more"));

        let mut done = latest;
        done.status = TaskStatus::Done;
        done.session_id = Some("sess".into());
        done.finished_at = Some(now_rfc3339());
        db.update_run(&done).unwrap();
        db.set_task_status(&task.id, TaskStatus::Done).unwrap();
        assert_eq!(
            db.get_task(&task.id).unwrap().unwrap().status,
            TaskStatus::Done
        );

        // cascade delete project removes tasks
        db.delete_project(&project.id).unwrap();
        assert!(db.get_task(&task.id).unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn grok_profile_uses_agent_stdio() {
        let (dir, db) = temp_db();
        seed_test_profiles(&db).unwrap();
        let grok = db
            .list_profiles()
            .unwrap()
            .into_iter()
            .find(|p| p.harness == HarnessKind::Grok)
            .expect("grok profile");
        assert_eq!(grok.args, vec!["agent", "stdio"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn migrate_legacy_grok_profile() {
        let (dir, db) = temp_db();
        let ts = now_rfc3339();
        let legacy = HarnessProfile {
            id: new_id(),
            name: "Grok Build".into(),
            harness: HarnessKind::Grok,
            command: "grok".into(),
            args: vec![],
            model: None,
            thought_level: None,
            discovered: Default::default(),
            system_prompt: String::new(),
            skills: vec![],
            mcp_servers: vec![],
            env: HashMap::new(),
            auto_approve: true,
            created_at: ts.clone(),
            updated_at: ts,
        };
        db.upsert_profile(&legacy).unwrap();
        migrate_harness_profiles(&db).unwrap();
        let fixed = db.get_profile(&legacy.id).unwrap().unwrap();
        assert_eq!(fixed.args, vec!["agent", "stdio"]);
        assert_eq!(fixed.name, "Grok Build (ACP)");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_command_finds_sh() {
        // `sh` should exist on unix
        #[cfg(unix)]
        {
            assert!(resolve_command(&["sh"]).is_some());
        }
        assert!(resolve_command(&["definitely-not-a-real-bin-xyz"]).is_none());
    }
}
