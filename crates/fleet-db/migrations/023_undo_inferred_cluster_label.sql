-- Undo 022's old backfill: container-only UI-paired hosts (`created_by` set)
-- must stay workspace-scoped. Bootstrap hosts keep `created_by IS NULL`.
UPDATE worker_hosts
SET labels = array_remove(labels, 'cluster')
WHERE advertised_container
  AND NOT advertised_host
  AND created_by IS NOT NULL
  AND 'cluster' = ANY (labels);
