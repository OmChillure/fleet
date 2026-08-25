-- Workspace minimum isolation for job placement.
-- Stored as the IsolationClass wire string: process | container | vm.
-- Default is the weakest class (process). Not a second enum.

ALTER TABLE workspaces
    ADD COLUMN min_isolation TEXT NOT NULL DEFAULT 'process';

ALTER TABLE workspaces
    ADD CONSTRAINT workspaces_min_isolation_check
    CHECK (min_isolation IN ('process', 'container', 'vm'));
