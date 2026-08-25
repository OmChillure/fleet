-- Per-runner runtime knobs the plane can set; the box applies them on settings_gen.
-- runtime_settings = desired (PATCH). runtime_reported = last heartbeat snapshot.

ALTER TABLE worker_hosts
  ADD COLUMN IF NOT EXISTS runtime_settings jsonb NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE worker_hosts
  ADD COLUMN IF NOT EXISTS runtime_reported jsonb NOT NULL DEFAULT '{}'::jsonb;
