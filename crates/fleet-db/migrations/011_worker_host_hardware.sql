-- Last-reported machine inventory (cores, RAM, OS). Utilization is a snapshot, not a timeseries.

ALTER TABLE worker_hosts
    ADD COLUMN hardware JSONB;
