-- Durable planner jobs, runner kind, partner pool, BYO cloud credentials.

ALTER TABLE worker_hosts
    ADD COLUMN runner_kind TEXT NOT NULL DEFAULT 'self_hosted';

ALTER TABLE worker_hosts
    ADD CONSTRAINT worker_hosts_runner_kind_check
    CHECK (runner_kind IN (
        'self_hosted',
        'fleet_cloud',
        'public_cloud',
        'partners'
    ));

CREATE INDEX worker_hosts_runner_kind_idx
    ON worker_hosts (workspace_id, runner_kind);

CREATE TABLE IF NOT EXISTS plan_jobs (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_by      UUID REFERENCES users(id) ON DELETE SET NULL,
    goal            TEXT NOT NULL,
    context         JSONB NOT NULL DEFAULT '{}'::jsonb,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'planning', 'accepted', 'failed')),
    plan_id         UUID,
    error           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX plan_jobs_workspace_idx
    ON plan_jobs (workspace_id, created_at DESC);

CREATE TABLE IF NOT EXISTS partner_offers (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    host_id         UUID NOT NULL REFERENCES worker_hosts(id) ON DELETE CASCADE,
    verified        BOOLEAN NOT NULL DEFAULT FALSE,
    enabled         BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (host_id)
);

CREATE TABLE IF NOT EXISTS partner_leases (
    id              UUID PRIMARY KEY,
    offer_id        UUID NOT NULL REFERENCES partner_offers(id) ON DELETE CASCADE,
    borrower_workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'ended', 'killed')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at        TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS partner_credits (
    workspace_id    UUID PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
    balance         BIGINT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cloud_credentials (
    id              UUID PRIMARY KEY,
    workspace_id    UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    provider        TEXT NOT NULL
                    CHECK (provider IN ('aws', 'gcp', 'azure', 'fleet')),
    ciphertext      BYTEA NOT NULL,
    host_id         UUID REFERENCES worker_hosts(id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX cloud_credentials_workspace_idx
    ON cloud_credentials (workspace_id, provider);
