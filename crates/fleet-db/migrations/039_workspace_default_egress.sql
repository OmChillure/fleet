-- Network posture for new jobs. Independent of default_run_mode
-- (on_pc vs in_container). `inherit` keeps the runner's own network.

ALTER TABLE workspaces
    ADD COLUMN default_egress TEXT NOT NULL DEFAULT 'inherit';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_default_egress_check
    CHECK (default_egress IN ('inherit', 'none', 'allowlist', 'open'));

ALTER TABLE workspaces
    ADD COLUMN default_egress_allowlist TEXT NOT NULL DEFAULT '';
