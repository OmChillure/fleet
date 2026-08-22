-- Worker-reported software identity (git sha + on-disk bin hashes).
-- Desired bits stay in the served dist manifest (later); this table only stores what the box said.

ALTER TABLE worker_hosts
    ADD COLUMN software_git_sha TEXT,
    ADD COLUMN software_bins JSONB,
    ADD COLUMN software_reported_at TIMESTAMPTZ,
    ADD COLUMN software_updating BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN auto_update BOOLEAN NOT NULL DEFAULT TRUE;
