-- Instance-wide concurrent-agent hard cap (default 128). Workspace
-- project_configs.max_agents is the live count (default 16).

CREATE TABLE instance_settings (
    id              SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    max_agents_cap  INT NOT NULL DEFAULT 128
        CHECK (max_agents_cap >= 1 AND max_agents_cap <= 1024),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO instance_settings (id, max_agents_cap)
VALUES (1, 128)
ON CONFLICT (id) DO NOTHING;

ALTER TABLE project_configs
    ALTER COLUMN max_agents SET DEFAULT 16;
