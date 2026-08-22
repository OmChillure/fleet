-- Stable identity of the physical box. Survives remove + re-pair.
ALTER TABLE worker_hosts
    ADD COLUMN machine_id UUID;

CREATE UNIQUE INDEX worker_hosts_ws_machine_id
    ON worker_hosts (workspace_id, machine_id)
    WHERE machine_id IS NOT NULL;
