-- Workspace OAuth *applications* (Fleet's client_id/secret at GitHub, Jira, …).
-- Distinct from integration_connections (user tokens after consent).
-- client_secret is stored as an AES-256-GCM envelope by fleet-integrations.
-- Numbered 005: origin/main used 004 for chat_conversations.

CREATE TABLE integration_oauth_apps (
    workspace_id     UUID NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    provider         TEXT NOT NULL,
    client_id        TEXT NOT NULL,
    client_secret    JSONB NOT NULL,
    created_by       UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, provider)
);

CREATE INDEX integration_oauth_apps_workspace_idx
    ON integration_oauth_apps (workspace_id);
