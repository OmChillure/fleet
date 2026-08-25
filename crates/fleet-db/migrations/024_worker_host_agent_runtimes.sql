-- Official agent CLIs last reported by fleet-host (no tokens).
ALTER TABLE worker_hosts
    ADD COLUMN IF NOT EXISTS agent_runtimes JSONB NOT NULL DEFAULT '[]'::jsonb;
