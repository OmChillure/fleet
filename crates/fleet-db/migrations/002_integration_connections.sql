-- Workspace-linked platform connections (GitHub, Jira, Slack, …).
-- Credentials live only in the control plane — never returned on list APIs
-- and never injected into sandboxes. Agents call external APIs via the
-- orchestrator proxy later; this table is the source of truth for tokens.

CREATE TABLE integration_connections (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    -- Catalog provider id: github | jira | slack | linear | gitlab | teams
    provider        TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'connected'
                    CHECK (status IN ('connected', 'error', 'disconnected')),
    -- Human-readable label (org name, site host, "personal", …)
    account_label   TEXT,
    -- Non-secret metadata (site_url, org, team id, scopes, …)
    config          JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Secret material (tokens, api keys). Control-plane only; never serialize
    -- in public API responses. Encrypt-at-rest is a follow-up.
    credentials     JSONB NOT NULL DEFAULT '{}'::jsonb,
    connected_by    UUID REFERENCES users (id) ON DELETE SET NULL,
    last_error      TEXT,
    connected_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    version         BIGINT NOT NULL DEFAULT 1
);

-- One connection row per provider per workspace
CREATE UNIQUE INDEX integration_connections_workspace_provider_uidx
    ON integration_connections (workspace_id, provider);

CREATE INDEX integration_connections_workspace_status_idx
    ON integration_connections (workspace_id, status)
    WHERE status = 'connected';
