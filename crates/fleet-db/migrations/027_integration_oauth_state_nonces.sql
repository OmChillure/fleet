-- One-time OAuth `state` JWT nonces. Connect callback inserts the nonce;
-- a later callback with the same nonce (replay, parallel, or a new JWT
-- carrying a used nonce) fails closed. Rows survive disconnect so a
-- replay cannot attach a second connection.

CREATE TABLE integration_oauth_state_nonces (
    nonce        TEXT PRIMARY KEY,
    workspace_id UUID NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    provider     TEXT NOT NULL,
    consumed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX integration_oauth_state_nonces_workspace_idx
    ON integration_oauth_state_nonces (workspace_id);
