-- Workspace envelope is the widest place and network a job may use.
-- A repo posture is the default for that codebase. Jobs with no repo
-- keep workspaces.default_run_mode and default_egress, then the envelope
-- clips anything looser.

ALTER TABLE workspaces
    ADD COLUMN run_envelope TEXT NOT NULL DEFAULT 'on_pc';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_run_envelope_check
    CHECK (run_envelope IN ('on_pc', 'in_container'));

ALTER TABLE workspaces
    ADD COLUMN egress_envelope TEXT NOT NULL DEFAULT 'open';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_egress_envelope_check
    CHECK (egress_envelope IN ('open', 'allowlist', 'none'));

CREATE TABLE repo_postures (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    repo TEXT NOT NULL,
    run_mode TEXT NOT NULL,
    egress_mode TEXT NOT NULL,
    egress_allowlist TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT repo_postures_workspace_repo_unique UNIQUE (workspace_id, repo),
    CONSTRAINT repo_postures_run_mode_check CHECK (run_mode IN ('on_pc', 'in_container')),
    CONSTRAINT repo_postures_egress_mode_check CHECK (
        egress_mode IN ('inherit', 'none', 'allowlist', 'open')
    )
);

CREATE INDEX repo_postures_workspace_idx ON repo_postures (workspace_id);
