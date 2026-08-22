-- Workspace invites: owner/admin invites by email; accept on signup/login.

CREATE TABLE workspace_invites (
    id                UUID PRIMARY KEY,
    workspace_id      UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    email_normalized  TEXT NOT NULL,
    role              TEXT NOT NULL
                      CHECK (role IN ('admin', 'member')),
    token             TEXT NOT NULL,
    invited_by        UUID NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ NOT NULL,
    accepted_at       TIMESTAMPTZ,
    accepted_user_id  UUID REFERENCES users (id) ON DELETE RESTRICT,
    revoked_at        TIMESTAMPTZ
);

CREATE UNIQUE INDEX workspace_invites_token_uidx ON workspace_invites (token);

CREATE UNIQUE INDEX workspace_invites_open_email_uidx
    ON workspace_invites (workspace_id, email_normalized)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;

CREATE INDEX workspace_invites_email_open_idx
    ON workspace_invites (email_normalized)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
