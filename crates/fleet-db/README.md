# fleet-db

Shared **Postgres** layer for Agent Fleet:

- SQL migrations (embedded)
- Connection pool helpers (`DATABASE_URL` — Neon, Supabase, local)
- Row models
- Repositories (auth sessions, users, workspaces, work claim queue)

Other crates (`fleet-auth`, future orchestrator) depend on this crate and
**must not** own their own migrations.

## Layout

```text
migrations/          # source of truth for schema
src/
  pool.rs            # connect + migrate
  models/            # User, AuthSession, WorkClaim, …
  repos/             # query helpers
```

## Usage

```rust
use fleet_db::{connect_and_migrate, repos};

let pool = connect_and_migrate(&std::env::var("DATABASE_URL")?).await?;
let user = repos::users::find_by_email_normalized(&pool, "a@b.com").await?;
```

## Concurrency notes

- UUID v7 PKs (app-generated)
- Auth: insert sessions; revoke by PK / token hash only
- Agents: `repos::work_claims::claim_next` uses **`FOR UPDATE SKIP LOCKED`**
- Optimistic `version` on mutable entities
- No shared counters on hot user rows

## Run migrations only

Migrations run automatically via `connect_and_migrate` on service startup.
To apply with the sqlx CLI against this crate:

```bash
# from workspace root, with DATABASE_URL set
cd crates/fleet-db && sqlx migrate run
```
