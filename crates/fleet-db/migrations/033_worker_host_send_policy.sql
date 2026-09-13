-- Who may send work to a Run-on-a-PC host.
-- owner_only (default) | allowlist | workspace
-- See docs/components/runners/run-on-pc-vs-container.md

ALTER TABLE worker_hosts
    ADD COLUMN send_policy TEXT NOT NULL DEFAULT 'owner_only';

ALTER TABLE worker_hosts
    ADD CONSTRAINT worker_hosts_send_policy_check
    CHECK (send_policy IN ('owner_only', 'allowlist', 'workspace'));

CREATE TABLE worker_host_send_allowlist (
    host_id  UUID NOT NULL REFERENCES worker_hosts (id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    added_by UUID REFERENCES users (id) ON DELETE RESTRICT,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (host_id, user_id)
);

CREATE INDEX worker_host_send_allowlist_user_idx
    ON worker_host_send_allowlist (user_id);

ALTER TABLE workspaces
    ADD COLUMN default_send_policy TEXT NOT NULL DEFAULT 'owner_only';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_default_send_policy_check
    CHECK (default_send_policy IN ('owner_only', 'allowlist', 'workspace'));
