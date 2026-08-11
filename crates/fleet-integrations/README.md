# fleet-integrations

Users **OAuth-connect** platforms (GitHub, GitLab, Slack, Jira, Linear, Figma, Notion,
Google Drive, Teams, Discord, Trello, Asana, …) in the browser.
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
| `GITLAB_CLIENT_ID` / `GITLAB_CLIENT_SECRET` | GitLab OAuth app |
| `SLACK_CLIENT_ID` / `SLACK_CLIENT_SECRET` | Slack app |
| `LINEAR_CLIENT_ID` / `LINEAR_CLIENT_SECRET` | Linear OAuth |
| `JIRA_CLIENT_ID` / `JIRA_CLIENT_SECRET` | Atlassian OAuth 2.0 (3LO) |
| `FIGMA_CLIENT_ID` / `FIGMA_CLIENT_SECRET` | Figma OAuth |
| `NOTION_CLIENT_ID` / `NOTION_CLIENT_SECRET` | Notion public integration |
| `GOOGLE_DRIVE_CLIENT_ID` / `GOOGLE_DRIVE_CLIENT_SECRET` | Google OAuth (Drive) |
| `TEAMS_CLIENT_ID` / `TEAMS_CLIENT_SECRET` | Microsoft Entra app (Teams/Graph) |
| `DISCORD_CLIENT_ID` / `DISCORD_CLIENT_SECRET` | Discord OAuth2 app |
| `TRELLO_CLIENT_ID` / `TRELLO_CLIENT_SECRET` | Trello API key + secret |
| `ASANA_CLIENT_ID` / `ASANA_CLIENT_SECRET` | Asana OAuth app |

Callback URL to register on each platform:

```text
{API_PUBLIC_BASE}/api/integrations/oauth/callback/{provider}
```

If client id/secret are missing, Connect still starts the provider flow
(GitHub App Manifest, or a one-time app form for Jira and others).

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

## Control-plane API proxy (chat / agents)

Layout under `src/proxy/`:

```text
proxy/
  mod.rs           # invoke() — decrypt + dispatch
  types.rs         # ProxyRequest / ProxyResult
  sanitize.rs
  apps/
    mod.rs         # register providers
    github/        client + tools/{reads,writes}
    gitlab/        client + tools/{reads,writes}
    jira/          client + tools/issues
    linear/        client + tools/{issues,writes}
    slack/         client + tools/conversations
    figma/         client + tools/files
    notion/        client + tools/pages
    google_drive/  client + tools/files
    teams/         client + tools/graph
    discord/       client + tools/me
    trello/        client + tools/boards
    asana/         client + tools/tasks
```

**Add a tool:** implement in `apps/<provider>/tools/`, add to that app’s
`OPERATIONS` + `invoke` match. No change needed in chat if using
`integration_query`; optional dedicated chat tool for UX.

Tokens are **never** returned to the browser or injected into sandboxes.
Chat and future agent tool proposals call `fleet_integrations::proxy::invoke`:

```rust
use fleet_integrations::proxy::{invoke, ProxyCaller, ProxyRequest};

let result = invoke(
    &pool,
    &cfg.credentials_key,
    ProxyRequest {
        workspace_id,
        provider: "github".into(),
        operation: "list_prs".into(),
        args: serde_json::json!({ "owner": "acme", "repo": "app" }),
        caller: ProxyCaller::Chat { user_id },
    },
).await;
// result.data is sanitized (no access_token / refresh_token)
```

| Provider | Operations |
| --- | --- |
| `github` (read) | `list_repos`, `list_issues`, `get_issue`, `list_prs`, `get_pr`, `get_file`, `list_commits` |
| `github` (write) | `put_file`, `commit_files`, `create_branch`, `create_pr`, `create_issue`, `add_comment` |
| `gitlab` | `list_repos`, `list_issues`, `get_issue`, `list_mrs`, `get_mr`, `get_file`, `list_commits`, `commit_files`, `create_branch`, `create_mr`, `create_issue`, `add_comment` |
| `jira` | `search_issues`, `get_issue`, `list_projects`, `create_issue`, `add_comment`, `list_transitions`, `transition_issue` |
| `linear` | `list_issues`, `get_issue`, `list_teams`, `create_issue`, `update_issue`, `add_comment` |
| `slack` | `list_channels`, `list_users`, `post_message`, `get_channel_history` |
| `figma` | `get_me`, `get_file`, `get_file_nodes`, `list_comments`, `list_team_projects`, `get_project_files` |
| `notion` | `search`, `get_page`, `get_database`, `query_database`, `get_block_children`, `create_page`, `append_blocks` |
| `google_drive` | `list_files`, `get_file`, `export_file` (readonly) |
| `teams` | `get_me`, `list_chats`, `list_teams`, `list_channels`, `list_chat_messages`, `send_chat_message`, `send_channel_message` |
| `discord` | `get_me`, `list_guilds` |
| `trello` | `list_boards`, `list_lists`, `list_cards`, `get_card`, `create_card`, `update_card`, `add_comment` |
| `asana` | `list_workspaces`, `list_projects`, `list_tasks`, `get_task`, `create_task`, `update_task`, `add_comment` |

Decrypt path: `credentials_for` → provider HTTP → `sanitize_value` → chat/agent.  
**Writes run only on the control plane** (chat tools `github_commit_files` / `github_create_pr`).  
Sandboxes never receive OAuth tokens. Arbitrary HTTP proxy is not allowed.
