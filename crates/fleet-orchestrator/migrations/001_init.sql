-- Agent Fleet orchestration baseline schema (design §Data Model)

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE projects (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE project_configs (
    project_id              UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    max_agents              INT NOT NULL DEFAULT 3,
    tick_interval_s         INT NOT NULL DEFAULT 5,
    heartbeat_stale_s       INT NOT NULL DEFAULT 60,
    spawn_timeout_s         INT NOT NULL DEFAULT 30,
    kill_grace_s            INT NOT NULL DEFAULT 15,
    default_task_timeout_s  INT NOT NULL DEFAULT 3600,
    default_max_retries     INT NOT NULL DEFAULT 2,
    retry_backoff_s         INT NOT NULL DEFAULT 10,
    on_task_fail            TEXT NOT NULL DEFAULT 'fail_run',
    auto_sync_external      BOOLEAN NOT NULL DEFAULT false,
    sandbox_backend         TEXT NOT NULL DEFAULT 'stub',
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE work_runs (
    id                      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id              UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    status                  TEXT NOT NULL,
    source                  TEXT NOT NULL,
    cancel_requested_at     TIMESTAMPTZ,
    orchestration_owned_at  TIMESTAMPTZ,
    first_spawn_at          TIMESTAMPTZ,
    terminal_at             TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX work_runs_project_status_idx ON work_runs (project_id, status);

CREATE TABLE work_plans (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    run_id          UUID NOT NULL REFERENCES work_runs(id) ON DELETE CASCADE,
    version         INT NOT NULL,
    status          TEXT NOT NULL,
    source          TEXT NOT NULL,
    body            JSONB NOT NULL,
    accepted_at     TIMESTAMPTZ,
    materialized_at TIMESTAMPTZ,
    in_force_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (run_id, version)
);
CREATE INDEX work_plans_pending_materialize_idx
    ON work_plans (project_id, status)
    WHERE materialized_at IS NULL;

CREATE TABLE board_tasks (
    id                      UUID PRIMARY KEY,
    project_id              UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    run_id                  UUID NOT NULL REFERENCES work_runs(id) ON DELETE CASCADE,
    plan_id                 UUID NOT NULL REFERENCES work_plans(id) ON DELETE CASCADE,
    plan_task_id            TEXT NOT NULL,
    title                   TEXT NOT NULL,
    summary                 TEXT NOT NULL DEFAULT '',
    spec_md                 TEXT NOT NULL DEFAULT '',
    status                  TEXT NOT NULL,
    depends_on              UUID[] NOT NULL DEFAULT '{}',
    priority                INT NOT NULL DEFAULT 0,
    worker_profile          TEXT NOT NULL,
    acceptance              JSONB NOT NULL DEFAULT '[]',
    external_ref            JSONB,
    timeout_s               INT,
    max_retries             INT NOT NULL DEFAULT 2,
    attempt                 INT NOT NULL DEFAULT 0,
    claimed_session_id      UUID,
    last_report_seq         INT NOT NULL DEFAULT 0,
    progress_pct            INT NOT NULL DEFAULT 0,
    block_reason            TEXT,
    hitl_notes              TEXT,
    hitl_artifacts          JSONB,
    hitl_resume_requested_at TIMESTAMPTZ,
    hitl_notified_at        TIMESTAMPTZ,
    hitl_resolved_at        TIMESTAMPTZ,
    failure_reason          TEXT,
    artifacts               JSONB,
    retry_not_before        TIMESTAMPTZ,
    assigned_at             TIMESTAMPTZ,
    started_at              TIMESTAMPTZ,
    terminal_at             TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, plan_id, plan_task_id)
);
CREATE INDEX board_tasks_project_status_priority_idx
    ON board_tasks (project_id, status, priority DESC);
CREATE INDEX board_tasks_project_run_idx ON board_tasks (project_id, run_id);

CREATE TABLE worker_sessions (
    id                  UUID PRIMARY KEY,
    project_id          UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    run_id              UUID NOT NULL REFERENCES work_runs(id) ON DELETE CASCADE,
    board_task_id       UUID NOT NULL REFERENCES board_tasks(id) ON DELETE CASCADE,
    attempt             INT NOT NULL,
    status              TEXT NOT NULL,
    runtime_backend     TEXT NOT NULL,
    runtime_handle      TEXT,
    worker_token_hash   TEXT NOT NULL,
    worker_profile      TEXT NOT NULL,
    last_heartbeat_at   TIMESTAMPTZ,
    progress_pct        INT NOT NULL DEFAULT 0,
    exit_reason         TEXT,
    kill_grace_deadline TIMESTAMPTZ,
    work_dir            TEXT,
    started_at          TIMESTAMPTZ,
    exited_at           TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (board_task_id, attempt)
);
CREATE INDEX worker_sessions_project_status_idx ON worker_sessions (project_id, status);

CREATE TABLE worker_reports (
    id              BIGSERIAL PRIMARY KEY,
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id      UUID NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    board_task_id   UUID NOT NULL REFERENCES board_tasks(id) ON DELETE CASCADE,
    seq             INT NOT NULL,
    kind            TEXT NOT NULL,
    payload         JSONB NOT NULL DEFAULT '{}',
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_at      TIMESTAMPTZ,
    UNIQUE (session_id, seq)
);
CREATE INDEX worker_reports_unapplied_idx
    ON worker_reports (project_id)
    WHERE applied_at IS NULL;

CREATE TABLE orchestration_events (
    id          BIGSERIAL PRIMARY KEY,
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    run_id      UUID REFERENCES work_runs(id) ON DELETE SET NULL,
    tick_id     UUID,
    event_type  TEXT NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ
);

CREATE TABLE idempotency_keys (
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key           TEXT NOT NULL,
    request_hash  TEXT NOT NULL,
    response      JSONB NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, key)
);

-- Phase 2 reserved (unused in MVP)
CREATE TABLE project_leases (
    project_id  UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    owner       TEXT NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
