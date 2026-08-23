-- Same cleanup as fleet-db 020, for DBs that only run this migrator.
-- IF EXISTS so a later fleet-db 020 (or the reverse) is a no-op.

DROP TABLE IF EXISTS project_leases;
DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS orchestration_events;
