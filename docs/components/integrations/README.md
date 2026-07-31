# Integrations

Users **sign in with OAuth** (GitHub, Slack, Jira, Linear) from the web UI.
Tokens live only in the control plane for AI / orchestrator use.

**Crate:** [`crates/fleet-integrations/`](../../../crates/fleet-integrations/)  
**UI:** `/{workspace_id}/integrations`

## Flow

```text
  User clicks "Connect with GitHub"
       │  full-page navigate (session cookie)
       ▼
  fleet-server oauth/start → 302 provider
       │
       ▼
  User authorizes on GitHub / Slack / …
       │
       ▼
  oauth/callback → exchange code → Postgres
       │
       ▼
  Redirect to UI integrations page (connected)
       │
       ▼
  Later: credentials_for(workspace, provider) for AI proxy
```

No personal access token paste. Env holds **Fleet’s OAuth app** credentials only.

## Env

See [`.env.example`](../../../.env.example):

- `API_PUBLIC_BASE`, `UI_PUBLIC_BASE`
- `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`
- `SLACK_*`, `LINEAR_*`, `JIRA_*`

## Related

- Product rule: secrets never enter sandboxes — [`product_vision.md`](../../product_vision.md)
