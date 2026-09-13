-- Run on a PC vs Run in a container (use case stamp).
-- See docs/components/runners/run-on-pc-vs-container.md

ALTER TABLE worker_hosts
    ADD COLUMN run_mode TEXT NOT NULL DEFAULT 'on_pc';

ALTER TABLE worker_hosts
    ADD CONSTRAINT worker_hosts_run_mode_check
    CHECK (run_mode IN ('on_pc', 'in_container'));

UPDATE worker_hosts
   SET run_mode = 'in_container'
 WHERE created_by IS NULL
    OR EXISTS (
         SELECT 1 FROM unnest(labels) l
         WHERE lower(trim(l)) = 'cluster'
       );

ALTER TABLE workspaces
    ADD COLUMN default_run_mode TEXT NOT NULL DEFAULT 'on_pc';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_default_run_mode_check
    CHECK (default_run_mode IN ('on_pc', 'in_container'));
