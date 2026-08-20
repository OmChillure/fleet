-- One-shot "Update now" while plane auto_update is off.
ALTER TABLE worker_hosts
    ADD COLUMN update_requested_at TIMESTAMPTZ;
