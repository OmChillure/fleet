# fleet-db

Shared **Postgres** layer for Agent Fleet:

- SQL migrations (embedded via `sqlx::migrate!`)
- Connection pool helpers (`DATABASE_URL` — Neon, Supabase, local)
- Row models
- Repositories (users, auth sessions, **workspaces**, work claim queue)

Other crates (`fleet-auth`, future orchestrator) depend on this crate and
**must not** own their own migrations.

---

## Layout

```text
migrations/          # source of truth for schema (001_init.sql, …)
src/
  pool.rs            # connect + migrate (TLS via workspace sqlx features)
  models/            # User, AuthSession, Workspace, WorkClaim, …
  repos/             # query helpers (no HTTP / JWT knowledge)
```

### Workspaces (multi-tenant foundation)

| Table | Role |
| --- | --- |
| `workspaces` | `id`, `slug` (unique), `name`, `version`, timestamps |
| `workspace_members` | `(workspace_id, user_id)` + `role` (`owner` / `admin` / `member`) |

Repo helpers (`repos::workspaces`):

| Function | Purpose |
| --- | --- |
| `create_personal_workspace_tx` | Insert workspace + owner membership in a tx |
| `create_owned` | Same, standalone transaction |
| `list_for_user` | Memberships ordered by `created_at` (first = default) |
| `find_for_user` | Membership check for one workspace |

Models: `Workspace`, `WorkspaceMember`, `WorkspaceWithRole`, `WorkspacePublic`.

---

## Usage

```rust
use fleet_db::{connect_and_migrate, repos};

let pool = connect_and_migrate(&std::env::var("DATABASE_URL")?).await?;
let user = repos::users::find_by_email_normalized(&pool, "a@b.com").await?;
let workspaces = repos::workspaces::list_for_user(&pool, user.unwrap().id).await?;
```

## Concurrency notes

- UUID v7 PKs (app-generated) — no sequence hotspots
- Auth: insert sessions; revoke by PK / token hash only
- Agents: `repos::work_claims::claim_next` uses **`FOR UPDATE SKIP LOCKED`**
- Optimistic `version` on mutable entities
- No shared counters on hot user rows

## TLS (Neon / Supabase)

Workspace `sqlx` enables `tls-rustls-ring`. Without a TLS feature, cloud
Postgres fails with *“TLS upgrade required … SQLx was built without TLS
support”*.

## Run migrations only

Migrations run automatically via `connect_and_migrate` on service startup.
To apply with the sqlx CLI against this crate:

```bash
# from workspace root, with DATABASE_URL set
cd crates/fleet-db && sqlx migrate run
```
