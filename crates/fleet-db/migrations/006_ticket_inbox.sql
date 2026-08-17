-- Ticket monitor + inbox: chat `@fleet monitor` / `@fleet finish this ticket`.
-- Workspace-scoped. Unique (workspace, provider, key) so polls upsert.

CREATE TABLE ticket_monitors (
    workspace_id    UUID PRIMARY KEY REFERENCES workspaces (id) ON DELETE RESTRICT,
    enabled         BOOLEAN NOT NULL DEFAULT false,
    providers       TEXT[] NOT NULL DEFAULT '{}',
    github_repos    TEXT[] NOT NULL DEFAULT '{}',
    jira_jql        TEXT,
    started_by      UUID REFERENCES users (id) ON DELETE RESTRICT,
    last_poll_at    TIMESTAMPTZ,
    last_error      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ticket_inbox (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    provider        TEXT NOT NULL,
    external_key    TEXT NOT NULL,
    title           TEXT NOT NULL,
    url             TEXT,
    state           TEXT NOT NULL DEFAULT 'open'
                    CHECK (state IN ('open', 'launched', 'done', 'ignored')),
    summary         TEXT NOT NULL DEFAULT '',
    run_id          UUID,
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, provider, external_key)
);

CREATE INDEX ticket_inbox_ws_state_idx ON ticket_inbox (workspace_id, state);
CREATE INDEX ticket_monitors_enabled_idx ON ticket_monitors (workspace_id) WHERE enabled;
