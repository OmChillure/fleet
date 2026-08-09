-- Workspace-scoped agent personas (roles / configs for workers).
-- No LLM credentials here — model_preference is a label for a later driver.

CREATE TABLE personas (
    id                 UUID PRIMARY KEY,
    workspace_id       UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    slug               TEXT NOT NULL,
    name               TEXT NOT NULL,
    description        TEXT NOT NULL DEFAULT '',
    icon               TEXT NOT NULL DEFAULT 'bot',
    role_prompt        TEXT NOT NULL DEFAULT '',
    worker_profile     TEXT NOT NULL DEFAULT 'coding',
    model_preference   TEXT,
    temperature        DOUBLE PRECISION,
    max_budget_usd     DOUBLE PRECISION,
    tools              JSONB NOT NULL DEFAULT '[]'::jsonb,
    is_starter         BOOLEAN NOT NULL DEFAULT false,
    is_default         BOOLEAN NOT NULL DEFAULT false,
    status             TEXT NOT NULL DEFAULT 'active'
                       CHECK (status IN ('active', 'archived')),
    created_by         UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    version            BIGINT NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, slug)
);

CREATE INDEX personas_workspace_status_idx
    ON personas (workspace_id, status);

CREATE INDEX personas_workspace_default_idx
    ON personas (workspace_id)
    WHERE is_default = true AND status = 'active';
