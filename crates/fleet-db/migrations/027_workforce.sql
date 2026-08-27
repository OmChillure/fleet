-- Workforce OS: durable goals + policy approvals.
-- Agents and sandboxes are disposable; the goal row is what survives overnight.
-- Humans approve only gated writes (merge, external comms).

CREATE TABLE workforce_goals (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    title           TEXT NOT NULL,
    goal_md         TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'open'
                    CHECK (status IN (
                        'open', 'acting', 'waiting_approval',
                        'completed', 'failed', 'cancelled'
                    )),
    run_id          UUID,
    case_id         UUID REFERENCES cases (id) ON DELETE SET NULL,
    created_by      UUID REFERENCES users (id) ON DELETE RESTRICT,
    last_error      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX workforce_goals_ws_status_idx
    ON workforce_goals (workspace_id, status, updated_at DESC);

CREATE INDEX workforce_goals_run_idx
    ON workforce_goals (run_id)
    WHERE run_id IS NOT NULL;

CREATE INDEX workforce_goals_case_idx
    ON workforce_goals (case_id)
    WHERE case_id IS NOT NULL;

CREATE TABLE policy_approvals (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    goal_id         UUID REFERENCES workforce_goals (id) ON DELETE SET NULL,
    task_id         UUID,
    kind            TEXT NOT NULL
                    CHECK (kind IN ('merge', 'external_comms', 'deploy')),
    provider        TEXT NOT NULL,
    operation       TEXT NOT NULL,
    args            JSONB NOT NULL DEFAULT '{}'::jsonb,
    summary         TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN (
                        'pending', 'approved', 'denied', 'executed', 'failed'
                    )),
    result          JSONB,
    decided_by      UUID REFERENCES users (id) ON DELETE RESTRICT,
    decided_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX policy_approvals_ws_status_idx
    ON policy_approvals (workspace_id, status, created_at DESC);

CREATE INDEX policy_approvals_goal_idx
    ON policy_approvals (goal_id)
    WHERE goal_id IS NOT NULL;
