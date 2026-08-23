# Integrations

Users **sign in with OAuth** from the web UI so each person connects **their**
workspace on each platform. Fleet only holds the **app** client id/secret;
per-user tokens stay encrypted in the control plane for AI / orchestrator use.

**Crate:** [`crates/fleet-integrations/`](../../../crates/fleet-integrations/)  
**UI:** `/{workspace_id}/integrations`  
**Developer README:** [`crates/fleet-integrations/README.md`](../../../crates/fleet-integrations/README.md)

---

## Available OAuth providers

| Provider | Env | Status |
| --- | --- | --- |
| GitHub | `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` | OAuth implemented |
| GitLab | `GITLAB_CLIENT_ID` / `GITLAB_CLIENT_SECRET` | OAuth implemented |
| Slack | `SLACK_CLIENT_ID` / `SLACK_CLIENT_SECRET` | OAuth implemented |
| Jira | `JIRA_CLIENT_ID` / `JIRA_CLIENT_SECRET` | OAuth implemented |
| Linear | `LINEAR_CLIENT_ID` / `LINEAR_CLIENT_SECRET` | OAuth implemented |
| Figma | `FIGMA_CLIENT_ID` / `FIGMA_CLIENT_SECRET` | OAuth implemented |
| Notion | `NOTION_CLIENT_ID` / `NOTION_CLIENT_SECRET` | OAuth implemented (public OAuth connection) |
| Google Drive | `GOOGLE_DRIVE_CLIENT_ID` / `GOOGLE_DRIVE_CLIENT_SECRET` | OAuth implemented |
| Microsoft Teams | `TEAMS_CLIENT_ID` / `TEAMS_CLIENT_SECRET` | OAuth implemented |
| Discord | `DISCORD_CLIENT_ID` / `DISCORD_CLIENT_SECRET` | OAuth implemented |
| Trello | `TRELLO_CLIENT_ID` / `TRELLO_CLIENT_SECRET` | OAuth implemented (API key + user token) |
| Asana | `ASANA_CLIENT_ID` / `ASANA_CLIENT_SECRET` | OAuth implemented |
| Bitbucket | `BITBUCKET_CLIENT_ID` / `BITBUCKET_CLIENT_SECRET` | OAuth implemented |
| Confluence | `CONFLUENCE_CLIENT_ID` / `CONFLUENCE_CLIENT_SECRET` | OAuth implemented (Atlassian 3LO) |

### Chat + apps

Say what you want in Chat. The assistant calls `integration_query` on the
control plane (Slack history, Notion pages, GitHub commit/PR, …) and
`launch_agent_plan` when work needs specialists. Tokens stay on the control
plane.

If a provider’s client id/secret is unset, **Connect** still starts the
provider flow (GitHub App Manifest, or a one-time OAuth app form for Jira
and others) and then the login + consent screen.

---

## Flow

```text
  User clicks Connect (new tab)
       │  session cookie on oauth/start
       ▼
  fleet-server → 302 provider authorize
       │
       ▼
  User authorizes (their account / pages)
       │
       ▼
  GET /api/integrations/oauth/callback/{provider}
       │  exchange code → AES-GCM encrypt → Postgres
       ▼
  Popup HTML postMessage(opener) + close
       │
       ▼
  UI marks Connected (optimistic + list poll)
       │
       ▼
  Later: credentials_for(pool, ws, provider, &key) for AI proxy
```

No personal access token paste. Users never see tokens — only OAuth consent.

### Notion (example)

1. [Notion Developer portal](https://app.notion.com/developers/connections) → **Public** connection with **OAuth**
2. Installable in: **Any workspace**
3. Redirect URI: `{API_PUBLIC_BASE}/api/integrations/oauth/callback/notion`
4. Put OAuth client id/secret in `.env` as `NOTION_CLIENT_*`
5. Users click **Connect** and grant **their** pages

---

## Env

See [`.env.example`](../../../.env.example):

| Variable | Purpose |
| --- | --- |
| `API_PUBLIC_BASE` | Callback origin (default `http://localhost:8080`) |
| `UI_PUBLIC_BASE` | UI origin for post-auth messaging |
| `JWT_SECRET` | OAuth `state` JWT + auth (≥ 32 chars) |
| `INTEGRATIONS_ENCRYPTION_KEY` | AES-256 key for tokens at rest (`openssl rand -hex 32`). Falls back to `JWT_SECRET` with a warning |
| `{PROVIDER}_CLIENT_ID` / `_SECRET` | Fleet’s OAuth app at each platform |

Callback to register on every provider:

```text
{API_PUBLIC_BASE}/api/integrations/oauth/callback/{provider}
```

---

## Credential storage & security

| Layer | Behavior |
| --- | --- |
| Browser | Never receives access/refresh tokens (public API strips them) |
| Postgres `credentials` | AES-256-GCM envelope `{ "_enc": "v1", "nonce", "ct" }` |
| Decrypt | Only via `credentials_for(..., &cfg.credentials_key)` on the control plane |
| Sandboxes | Must not receive raw tokens (product rule) |
| OAuth CSRF | Signed short-lived `state` JWT (`typ: oauth_state`) |
| Live APIs | `fleet_integrations::proxy::invoke` — allowlisted ops for all OAuth apps; sanitized results only |

Legacy plaintext rows still load until the user reconnects.

### Chat proxy tools

| Tool | Behavior |
| --- | --- |
| `integration_query` | Control-plane decrypt → provider API → sanitized JSON (no secrets) |
| `launch_agent_plan` | Multi-task fan-out; optional `context_from` embeds proxy results into task specs |
| `launch_agent_task` | Single task + multi-tick; sandboxes destroyed after exit |

Ops (all via `integration_query` — the model invents the workflow, we do not
register playbooks): GitHub and GitLab read/write; Slack channels/messages/threads;
Notion search/pages/blocks; Jira issues + transitions; Linear issues; Figma files;
Drive list/export; Teams chat/channel send; Discord identity + guilds;
Trello boards/cards; Asana tasks; Bitbucket repos/PRs/issues; Confluence pages.
All catalog providers above have OAuth connect. If client id/secret is unset, Connect still starts a one-time app form then consent.

---

## API

| Method | Path |
| --- | --- |
| `GET` | `/api/workspaces/{id}/integrations` |
| `GET` | `/api/workspaces/{id}/integrations/{provider}/oauth/start` |
| `GET` | `/api/integrations/oauth/callback/{provider}` |
| `DELETE` | `/api/workspaces/{id}/integrations/{provider}` |

---

## Related

- Crate guide: [`crates/fleet-integrations/README.md`](../../../crates/fleet-integrations/README.md)
- UI: [`docs/components/ui/README.md`](../ui/README.md)
- Product rule (secrets out of sandboxes): [`product_vision.md`](../../product_vision.md)
