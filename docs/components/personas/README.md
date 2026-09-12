# Personas

Workspace-scoped **agent roles**: name, role prompt, worker profile, model preference, tools, budget.

- **Design plan:** [plan.html](./plan.html) (open in a browser)
- **API:** mounted by `fleet-auth` under `/api/workspaces/{id}/personas`
- **UI:** Agents page (`/{workspace_id}/agents`)
- **Schema:** `fleet-db` migration `003_personas.sql`

No LLM is invoked from the personas HTTP API — `model_preference` is configuration. At spawn, `resolve_for_workspace` writes the catalog role prompt into `PERSONA.md` and uses `model_preference` when the run has no chat model.

See also [development.md](../../../development.md).
