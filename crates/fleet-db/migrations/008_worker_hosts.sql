-- Paired worker hosts (create-then-pair). See docs/components/worker-host/README.md.

CREATE TABLE worker_hosts (
    id                   UUID PRIMARY KEY,
    workspace_id         UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    name                 TEXT NOT NULL,
    status               TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending','online','disabled','error')),
    enabled              BOOLEAN NOT NULL DEFAULT TRUE,
    paused               BOOLEAN NOT NULL DEFAULT FALSE,
    drain                BOOLEAN NOT NULL DEFAULT FALSE,
    allow_host           BOOLEAN NOT NULL DEFAULT TRUE,
    allow_container      BOOLEAN NOT NULL DEFAULT TRUE,
    max_concurrent       INT NOT NULL DEFAULT 4
                         CHECK (max_concurrent >= 1 AND max_concurrent <= 256),
    max_job_timeout_s    INT NOT NULL DEFAULT 3600 CHECK (max_job_timeout_s >= 60),
    labels               TEXT[] NOT NULL DEFAULT '{}',
    machine_token_hash   TEXT,
    advertised_host      BOOLEAN NOT NULL DEFAULT FALSE,
    advertised_container BOOLEAN NOT NULL DEFAULT FALSE,
    platform             TEXT,
    hostname             TEXT,
    last_seen_at         TIMESTAMPTZ,
    last_error           TEXT,
    created_by           UUID REFERENCES users (id) ON DELETE RESTRICT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    paired_at            TIMESTAMPTZ,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    current_jobs         INT NOT NULL DEFAULT 0 CHECK (current_jobs >= 0),
    advertised_max_concurrent INT CHECK (
                             advertised_max_concurrent IS NULL
                             OR (advertised_max_concurrent >= 1
                                 AND advertised_max_concurrent <= 256)
                         )
);

CREATE INDEX worker_hosts_ws_idx ON worker_hosts (workspace_id, status);

CREATE TABLE worker_host_pair_tokens (
    id           UUID PRIMARY KEY,
    host_id      UUID NOT NULL REFERENCES worker_hosts (id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    token_hash   TEXT NOT NULL UNIQUE,
    expires_at   TIMESTAMPTZ NOT NULL,
    used_at      TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
