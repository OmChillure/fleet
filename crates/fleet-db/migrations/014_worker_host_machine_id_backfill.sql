-- Paired rows without a host-reported machine.id still need a durable id
-- so re-pair / UI can treat one box as one worker. Use the row id.
UPDATE worker_hosts
SET machine_id = id
WHERE machine_id IS NULL
  AND paired_at IS NOT NULL;
