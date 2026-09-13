-- Instance administrators (cross-workspace). Distinct from workspace
-- membership role `admin`. Default is not an instance admin.

ALTER TABLE users
    ADD COLUMN instance_admin BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX users_instance_admin_idx
    ON users (id)
    WHERE instance_admin = TRUE;
