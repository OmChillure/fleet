# Personas

Two catalogs, two names. They are not the same JSON.

## Workspace personas

Workspace-scoped **agent roles**: name, role prompt, worker profile, model preference, tools, budget. Seed keeps **8 starters**.

- **Design plan:** [plan.html](./plan.html) (open in a browser)
- **API:** mounted by `fleet-auth` under `/api/workspaces/{id}/personas`
- **UI:** Agents page (`/{workspace_id}/agents`), **Workspace personas** section
- **Schema:** `fleet-db` migration `003_personas.sql`

No LLM is invoked from this HTTP API — `model_preference` is configuration.

## Roster specialists

Agency-agents cards (division, emoji, vibe) from
`GET /api/workspaces/{id}/fleet/personas`. `worker_profile` on launch selects
one. Listed on the Agents page under **Roster specialists**.

Chat tool `list_agent_personas` returns specialists as the main list.
Workspace personas, when included, are a separate `workspace_personas` array
— never mixed into the specialist list.

At spawn, `resolve_for_workspace` writes a workspace-persona role prompt into
`PERSONA.md` when the slug matches; otherwise it uses the roster specialist.
`model_preference` applies when the run has no chat model.

See also [development.md](../../../development.md).
