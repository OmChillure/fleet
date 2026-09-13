-- Workspace vendor API keys for Run-in-a-container guests.
-- Ciphertext is an already-encrypted envelope (same as integrations).

CREATE TABLE workspace_llm_keys (
    id            UUID PRIMARY KEY,
    workspace_id  UUID NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    provider      TEXT NOT NULL,
    ciphertext    JSONB NOT NULL,
    created_by    UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at    TIMESTAMPTZ
);

CREATE UNIQUE INDEX workspace_llm_keys_one_active
    ON workspace_llm_keys (workspace_id, provider)
    WHERE revoked_at IS NULL;
