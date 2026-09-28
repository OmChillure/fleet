-- The API is not a runner. Isolation lives on paired fleet-host.
ALTER TABLE project_configs DROP COLUMN IF EXISTS sandbox_backend;
