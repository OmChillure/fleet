-- Runner isolation is a property of the host: native (v1) vs sandboxed
-- (placeholder toggle; not assignable — no k8s executor in v1).
-- Existing rows default to native. Table stays worker_hosts.

ALTER TABLE worker_hosts
    ADD COLUMN isolation TEXT NOT NULL DEFAULT 'native';

ALTER TABLE worker_hosts
    ADD CONSTRAINT worker_hosts_isolation_check
    CHECK (isolation IN ('native', 'sandboxed'));
