-- Control plane as runner #1. Not a paired fleet-host row: no heartbeat,
-- lease, or machine token. Settings persist per workspace. Tick still uses
-- local SANDBOX_BACKEND when this runner is taking native jobs; assignment
-- into the remote host pool is a later TODO.

CREATE TABLE workspace_plane_runners (
    workspace_id    UUID PRIMARY KEY REFERENCES workspaces (id) ON DELETE CASCADE,
    name            TEXT NOT NULL DEFAULT 'Control plane',
    allow_host      BOOLEAN NOT NULL DEFAULT TRUE,
    allow_container BOOLEAN NOT NULL DEFAULT TRUE,
    max_concurrent  INTEGER NOT NULL DEFAULT 4
                    CHECK (max_concurrent >= 1 AND max_concurrent <= 256),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
