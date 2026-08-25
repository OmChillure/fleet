-- In-cluster bootstrap hosts serve every workspace (placement, not a new isolation).
UPDATE worker_hosts
SET labels = CASE
        WHEN labels IS NULL OR cardinality(labels) = 0 THEN ARRAY['cluster']
        WHEN NOT ('cluster' = ANY (labels)) THEN labels || ARRAY['cluster']
        ELSE labels
    END
WHERE advertised_container
  AND NOT advertised_host;
