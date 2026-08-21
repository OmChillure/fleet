-- Apply-and-ack: plane bumps settings_gen; the box echoes it on heartbeat.
ALTER TABLE worker_hosts
    ADD COLUMN settings_gen INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN settings_ack_gen INTEGER NOT NULL DEFAULT 0;
