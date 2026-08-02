# fleet-integrations

Users **OAuth-connect** platforms (GitHub, Slack, Jira, Linear, Figma, Notion,
Google Drive, Teams, Discord, …) in the browser.
Access tokens are stored per workspace for the control plane / AI — never PATs
pasted into the UI, never secrets inside sandboxes.

## Flow

```text
UI "Connect with GitHub" (new tab)
  → GET /api/workspaces/{ws}/integrations/github/oauth/start  (session cookie)
  → 302 provider authorize
  → user Approves (their account)
  → GET /api/integrations/oauth/callback/github?code&state
  → exchange code → AES-GCM encrypt → Postgres
  → popup postMessage(opener) + close
  → UI marks Connected (optimistic + list poll)
```

## Env (Fleet’s OAuth apps)

| Variable | Purpose |
| --- | --- |
| `API_PUBLIC_BASE` | Redirect URI origin (default `http://localhost:8080`) |
| `UI_PUBLIC_BASE` | Where to land after connect (default `http://localhost:3030`) |
| `JWT_SECRET` | Signs session JWTs **and** OAuth `state` (min 32 chars) |
| `INTEGRATIONS_ENCRYPTION_KEY` | AES-256 key for OAuth tokens at rest (min 32 chars; prefer `openssl rand -hex 32`). Falls back to `JWT_SECRET` if unset |
| `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` | GitHub OAuth App |
| `SLACK_CLIENT_ID` / `SLACK_CLIENT_SECRET` | Slack app |
| `LINEAR_CLIENT_ID` / `LINEAR_CLIENT_SECRET` | Linear OAuth |
| `JIRA_CLIENT_ID` / `JIRA_CLIENT_SECRET` | Atlassian OAuth 2.0 (3LO) |
| `FIGMA_CLIENT_ID` / `FIGMA_CLIENT_SECRET` | Figma OAuth |
| `NOTION_CLIENT_ID` / `NOTION_CLIENT_SECRET` | Notion public integration |
| `GOOGLE_DRIVE_CLIENT_ID` / `GOOGLE_DRIVE_CLIENT_SECRET` | Google OAuth (Drive) |
| `TEAMS_CLIENT_ID` / `TEAMS_CLIENT_SECRET` | Microsoft Entra app (Teams/Graph) |
| `DISCORD_CLIENT_ID` / `DISCORD_CLIENT_SECRET` | Discord OAuth2 app |

Callback URL to register on each platform:

```text
{API_PUBLIC_BASE}/api/integrations/oauth/callback/{provider}
```

If client id/secret are missing, the UI shows **Not configured** (server still runs).

## API

| Method | Path |
| --- | --- |
| `GET` | `/api/workspaces/{id}/integrations` |
| `GET` | `/api/workspaces/{id}/integrations/{provider}/oauth/start` |
| `GET` | `/api/integrations/oauth/callback/{provider}` |
| `DELETE` | `/api/workspaces/{id}/integrations/{provider}` |

## Credential storage

OAuth access/refresh tokens are **AES-256-GCM encrypted** before insert into
`integration_connections.credentials`. Stored shape:

```json
{ "_enc": "v1", "nonce": "<hex>", "ct": "<hex ciphertext+tag>" }
```

- Never returned on list/public APIs
- Decrypt only via `credentials_for(pool, workspace_id, provider, &key)`
- Legacy plaintext rows still decrypt as-is until the user reconnects

## AI later

```rust
use fleet_integrations::{credentials_for, IntegrationsConfig};
let cfg = IntegrationsConfig::from_env()?;
let creds = credentials_for(&pool, workspace_id, "github", &cfg.credentials_key).await?;
```
