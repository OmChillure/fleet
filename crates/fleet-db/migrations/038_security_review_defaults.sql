-- Safer defaults for new workspaces / hosts. Existing rows are left unchanged.
ALTER TABLE workspaces
    ALTER COLUMN min_isolation SET DEFAULT 'container';

ALTER TABLE worker_hosts
    ALTER COLUMN auto_update SET DEFAULT FALSE;
