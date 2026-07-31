# Agent Fleet — vision artifacts

| Path | Description |
|------|-------------|
| `hero-fleet-concept.jpg` | Concept illustration — operator hub + worker fleet |
| `hitl-verification-concept.jpg` | Concept illustration — human-in-the-loop verification |
| `diagrams/*.mmd` | Mermaid sources (also embedded in `../product_vision_tarun.html`) |
| `diagrams/organization-project-hierarchy.mmd` | Organization → project → orchestrator / docs / board / fleet |
| `prototypes/00-projects.html` | Organization project list & switcher |
| `prototypes/05-starter-personas.html` | Create project from starter persona |
| `prototypes/01–04-*.html` | Project-scoped dashboard, board, documents, config |
| `diagrams/starter-personas.mmd` | Persona catalog → project seeds |
| `diagrams/org-board-integrations.mmd` | Org-level Jira/Monday/… → project mappings |
| `prototypes/06-org-integrations.html` | Organization board integrations UI |
| `sandbox-orchestration-design.html` | Planning vs orchestration design (flow, architecture, checkpoints) |
| `orchestration-service-design.md` | Orchestration service design (reconcile tick, data model, API, PR plan; **Rust** impl) |
| `diagrams/task-initiation-flow.mmd` | User initiates work → plan → orch handoff (CP1–CP10) |
| `diagrams/planning-orchestration-architecture.mmd` | Architecture: plan jobs vs reconcile loop |
| `diagrams/work-unit-states.mmd` | WorkRun / WorkPlan / BoardTask / WorkerSession states |
| `diagrams/orchestrator-reconcile-loop.mmd` | Pure-logic orchestrator tick |
| `diagrams/planning-handoff-sequence.mmd` | Sequence: initiate → plan → accept → workers |

## Domain model (short)

- **Organization** — org boundary; many projects; members & policies.
- **Project** — own **orchestrator**, document tree, board, config, worker fleet.
- **Documents** — hierarchical tree under each project.
- **Board integrations** — org-level Jira, Monday.com, Linear, Asana, Trello, Azure Boards; projects map boards.
- **Starter personas** — 8 samples: software, sales, marketing, customer support, analysis, DevOps, product discovery, content/docs (+ blank).

Open `../product_vision_tarun.html` in a browser (needs network for Mermaid CDN).
