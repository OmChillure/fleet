# fleet-integrations

Users **OAuth-connect** platforms (GitHub, Slack, Jira, Linear) in the browser.
Access tokens are stored per workspace for the control plane / AI — never PATs
pasted into the UI, never secrets inside sandboxes.

## Flow

```text
UI "Connect with GitHub"
  → GET /api/workspaces/{ws}/integrations/github/oauth/start  (session cookie)
  → 302 GitHub authorize
  → user Approves
  → GET /api/integrations/oauth/callback/github?code&state
  → exchange code → store credentials
  → 303 UI /{ws}/integrations?connected=github
```

## Env (Fleet’s OAuth apps)

| Variable | Purpose |
| --- | --- |
| `API_PUBLIC_BASE` | Redirect URI origin (default `http://localhost:8080`) |
| `UI_PUBLIC_BASE` | Where to land after connect (default `http://localhost:3030`) |
| `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` | GitHub OAuth App |
| `SLACK_CLIENT_ID` / `SLACK_CLIENT_SECRET` | Slack app |
| `LINEAR_CLIENT_ID` / `LINEAR_CLIENT_SECRET` | Linear OAuth |
| `JIRA_CLIENT_ID` / `JIRA_CLIENT_SECRET` | Atlassian OAuth 2.0 (3LO) |

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

## AI later

```rust
use fleet_integrations::credentials_for;
let creds = credentials_for(&pool, workspace_id, "github").await?;
```
