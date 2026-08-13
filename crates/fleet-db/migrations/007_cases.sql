-- Generalized durable cases: backend owns state + wake schedule; LLM decides at
-- wake points; integrations/tools plug in later. Coding fleets are optional
-- executors (cases.run_id → orchestrator work run).

CREATE TABLE cases (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    kind            TEXT NOT NULL DEFAULT 'generic',
    title           TEXT NOT NULL,
    policy_md       TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'open'
                    CHECK (status IN (
                        'open', 'waiting', 'deciding', 'acting',
                        'completed', 'failed', 'cancelled'
                    )),
    stage           TEXT NOT NULL DEFAULT 'start',
    context         JSONB NOT NULL DEFAULT '{}'::jsonb,
    next_wake_at    TIMESTAMPTZ,
    last_error      TEXT,
    run_id          UUID,
    decision_count  INT NOT NULL DEFAULT 0,
    created_by      UUID REFERENCES users (id) ON DELETE RESTRICT,
    version         BIGINT NOT NULL DEFAULT 1,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX cases_wake_idx
    ON cases (next_wake_at)
    WHERE status IN ('open', 'waiting') AND next_wake_at IS NOT NULL;

CREATE INDEX cases_ws_status_idx ON cases (workspace_id, status);

CREATE INDEX cases_acting_run_idx
    ON cases (run_id)
    WHERE status = 'acting' AND run_id IS NOT NULL;

CREATE TABLE case_events (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    case_id         UUID NOT NULL REFERENCES cases (id) ON DELETE CASCADE,
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    kind            TEXT NOT NULL,
    payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX case_events_case_idx ON case_events (case_id, created_at);
