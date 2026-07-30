-- Agent Fleet initial schema
--
-- Design goals for high concurrency (1k+ parallel agents / workers):
-- 1. UUID v7 primary keys generated in the app — no sequence hotspots.
-- 2. No shared counters on hot rows (no "active_sessions" on users).
-- 3. Auth sessions are insert-heavy; revoke is a single-row UPDATE by PK.
-- 4. Work claim queue uses FOR UPDATE SKIP LOCKED (see docs) — no deadlocks
--    from multi-agent polling; each agent locks a different ready row.
-- 5. Optimistic concurrency via `version` on mutable entities.
-- 6. FKs avoid ON DELETE CASCADE on high-churn tables (prefer RESTRICT + soft
--    status) so mass deletes do not lock parent rows under load.
-- 7. Partial indexes keep active-set lookups cheap without bloating dead rows.
-- 8. Safe for Neon / Supabase Postgres (no superuser-only extensions required).

-- ---------------------------------------------------------------------------
-- Identity
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id              UUID PRIMARY KEY,
    email           TEXT NOT NULL,
    -- Canonical form for uniqueness (always lower(trim(email)))
    email_normalized TEXT NOT NULL,
    display_name    TEXT,
    password_hash   TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'disabled', 'pending')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Optimistic concurrency for profile/password updates
    version         BIGINT NOT NULL DEFAULT 1
);

CREATE UNIQUE INDEX users_email_normalized_uidx ON users (email_normalized);
CREATE INDEX users_status_idx ON users (status) WHERE status = 'active';

-- ---------------------------------------------------------------------------
-- Multi-tenant foundation (low write rate)
-- ---------------------------------------------------------------------------

CREATE TABLE workspaces (
    id          UUID PRIMARY KEY,
    slug        TEXT NOT NULL,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    version     BIGINT NOT NULL DEFAULT 1
);

CREATE UNIQUE INDEX workspaces_slug_uidx ON workspaces (slug);

CREATE TABLE workspace_members (
    workspace_id UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    role         TEXT NOT NULL
                 CHECK (role IN ('owner', 'admin', 'member')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, user_id)
);

-- Reverse lookup: "workspaces for this user" without scanning membership table
CREATE INDEX workspace_members_user_id_idx ON workspace_members (user_id);

-- ---------------------------------------------------------------------------
-- Auth sessions / refresh tokens (medium-high churn)
--
-- Concurrency notes:
-- * Login inserts one row; never updates a global user lock.
-- * Refresh rotates by PK (id) + expected refresh_token_hash — single row.
-- * Reuse detection: if a revoked family's hash is presented again, revoke
--   the whole family (app logic); index supports family scans.
-- * Do not store raw refresh tokens — only SHA-256 hashes.
-- ---------------------------------------------------------------------------

CREATE TABLE auth_sessions (
    id                    UUID PRIMARY KEY,
    user_id               UUID NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    -- SHA-256 of the opaque refresh token (32 bytes)
    refresh_token_hash    BYTEA NOT NULL,
    -- Rotation family: all rotations share one family_id
    family_id             UUID NOT NULL,
    user_agent            TEXT,
    ip                    INET,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Sliding / idle expiry (extended on refresh)
    expires_at            TIMESTAMPTZ NOT NULL,
    -- Hard cap for the session family
    absolute_expires_at   TIMESTAMPTZ NOT NULL,
    revoked_at            TIMESTAMPTZ,
    last_seen_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fast lookup on login refresh (active tokens only)
CREATE UNIQUE INDEX auth_sessions_refresh_hash_uidx
    ON auth_sessions (refresh_token_hash)
    WHERE revoked_at IS NULL;

CREATE INDEX auth_sessions_user_active_idx
    ON auth_sessions (user_id, expires_at)
    WHERE revoked_at IS NULL;

CREATE INDEX auth_sessions_family_idx
    ON auth_sessions (family_id);

-- Expiry sweeper (background job can DELETE WHERE expires_at < now())
CREATE INDEX auth_sessions_expires_idx
    ON auth_sessions (expires_at);

-- ---------------------------------------------------------------------------
-- Work claim queue — foundation for 1000 parallel agents
--
-- Poll pattern (single statement, deadlock-free under concurrency):
--
--   WITH cte AS (
--     SELECT id FROM work_claims
--     WHERE status = 'ready'
--       AND available_at <= now()
--       AND workspace_id = $workspace
--     ORDER BY available_at, created_at
--     FOR UPDATE SKIP LOCKED
--     LIMIT 1
--   )
--   UPDATE work_claims w
--   SET status = 'leased',
--       lease_owner = $agent_id,
--       lease_token = $token,
--       lease_expires_at = now() + interval '60 seconds',
--       attempts = attempts + 1,
--       version = version + 1,
--       updated_at = now()
--   FROM cte
--   WHERE w.id = cte.id
--   RETURNING w.*;
--
-- SKIP LOCKED means agents never wait on each other's row locks → no deadlock
-- chains from the claim path. Heartbeats update by (id, lease_token) only.
-- ---------------------------------------------------------------------------

CREATE TABLE work_claims (
    id                UUID PRIMARY KEY,
    workspace_id      UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    kind              TEXT NOT NULL,
    payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    status            TEXT NOT NULL DEFAULT 'ready'
                      CHECK (status IN ('ready', 'leased', 'done', 'failed', 'dead')),
    lease_owner       UUID,
    lease_token       UUID,
    lease_expires_at  TIMESTAMPTZ,
    attempts          INT NOT NULL DEFAULT 0,
    max_attempts      INT NOT NULL DEFAULT 5,
    available_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error        TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    version           BIGINT NOT NULL DEFAULT 1
);

-- Hot path: agents poll ready work per workspace
CREATE INDEX work_claims_poll_idx
    ON work_claims (workspace_id, available_at, created_at)
    WHERE status = 'ready';

-- Lease recovery: reclaim expired leases without scanning all rows
CREATE INDEX work_claims_lease_expiry_idx
    ON work_claims (lease_expires_at)
    WHERE status = 'leased';

CREATE INDEX work_claims_workspace_status_idx
    ON work_claims (workspace_id, status);

-- ---------------------------------------------------------------------------
-- Append-only audit / event log (optional foundation)
-- Never UPDATE these rows — agents append; readers use time/id ranges.
-- ---------------------------------------------------------------------------

CREATE TABLE audit_events (
    id            UUID PRIMARY KEY,
    workspace_id  UUID,
    actor_user_id UUID,
    actor_kind    TEXT NOT NULL DEFAULT 'user'
                  CHECK (actor_kind IN ('user', 'agent', 'system')),
    event_type    TEXT NOT NULL,
    payload       JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_workspace_time_idx
    ON audit_events (workspace_id, created_at DESC);

CREATE INDEX audit_events_type_time_idx
    ON audit_events (event_type, created_at DESC);
