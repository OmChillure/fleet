Team vision - Agent Fleet:

We are building a platform that integrates flawlessly to existing code development workflows and facilitates delegating tickets/tasks to groups of AI agents. 
The agents must run on sandboxed environments starting with no internet access and gradually add more permissive rules like allowlisting or allowing package registries. It is not recommended to allow full internet access. 
The sandboxes can be either containers or VMs. Containers are leaner and more efficient, but sometimes VMs are required - for example, to run Android emulators. Multiple agents can work on the same sandbox.
Secrets must never enter the sandboxes. Requests to mcp servers and external APIs should be proxied in the orchestration layer where the secrets are added to the request. 


Broken down requirements:

Planes:

1. Main user interface - a Web app where devs configure how the platform should behave
2. Orchestration layer - grabs tasks from the integrations and spawns agents to work on them
3. Runtime - 
4. HITL - makes the developer feel part of the loop and like how things happen


Integrations:

* Task control / kanban
    * Jira
    * Trello
    * Asana
    * Linear
    * github
    * gitlab
* Code versioning & DevOps 
    * GitHub
    * GitLab
    * Whatever dev teams use to deploy and trigger CI/CD pipelines
* HITL requests
    * Slack
    * Teams
* Notifications
    * Slack
    * Email
    * Teams

Sandbox environments:

* Kubernetes
* docker?
* some VM orchestrator



Subteams:


Team Usability & Integrations
Team Orchestration
Team Sandbox

Responsibilities:

* Team Usability & Integrations (@omchillure, @Vamsee Kiran D) - Makes sure that the product is good to use and integrates well with current developer workflows
    * Who should be here: 
        * Someone who currently works with a team of developers
        * Someone who has hand-on experience with the current industry standard software
        * Someone who participates in package or product shipping to production
        * Someone with UI/UX expertise
        * Someone who is avid about orchestrating agents
* Team Orchestration (@Daniel Vrátil, @Eugene Pyvovarov) - Makes sure that the tasks handed to agents are well written and their output satisfies what was expected in the task. 
    * Who should be here: 
        * Someone who values standardization and reproducible workflows
        * Someone who has experience with agent orchestration
        * .
* Team Sandbox - Makes sure that the agents spawn, have everything they need to do their job, and do lot leave residue after they are done. 
        * @Tarun Gaba - Lifecycle management, state machine
        * @Viny Kriesang - Sandbox
    * Who should be here:
        * Someone with experience with container orchestration (Kubernetes, etc)
        * Someone with experience with container / VM management in cloud services (AWS, GCP, Azure)
        * Someone with experience with container / VM management in house
        * Someone with experience in networking



Each person expertise:
Tarun: Backend/infra/devops
Eugene: fullstack, infra/devops, experience with similar system for my own, running/controlling agents
Vamsee: Project Management, Fullstack (Vibe Coding) 
Om: backend, infra, distributed systems, low level system.
Viny: self-hosting infra, devops, networking, fullstack
Daniel: Backend/infra, experience with the orchestration part/driving of coding agent part of a similar system






This is a temporary vision, feel free to update whatever feels good.
Stack :  Rust 

@Vamsee Kiran D’s current PRODUCT vision:
Agent Fleet is the operating system for AI workforces.
We are building a platform that enables individuals and teams to operate AI workers as a reliable workforce rather than isolated assistants. Agent Fleet allows users to define business or engineering outcomes, while the platform plans, coordinates, executes, monitors, and continuously improves the work through fleets of specialized AI agents working together under human supervision.
Instead of focusing on individual prompts or single-agent automation, Agent Fleet provides the operational layer required to deploy AI workers safely at production scale. It manages orchestration, workflow execution, shared context, policies, approvals, observability, evaluation, and recovery so that hundreds or thousands of agents can operate as one coordinated workforce.
The platform is model-agnostic and supports local and cloud models through a common execution layer. It is designed for software engineering first, while providing a foundation that naturally extends to research, operations, sales, customer support, data analysis, and other enterprise workflows.

1. Our long-term vision is to become the operating system for AI workforces, where organizations manage AI workers with the same confidence, visibility, and governance that they manage human teams today.
2. Primary user: We'll Start with Small software teams (1–20 developers); smallest group with the strongest need and the fastest feedback cycle. "Agent Fleet enables small software teams to reliably run and supervise hundreds of long-running AI coding agents working toward a shared engineering goal."
3. Engineering centerpiece: Keeping hundreds or thousands of autonomous workers alive, coordinated and recoverable.
4. Architecture Principle: Every agent is disposable. Every workflow is durable.
Workers can crash; Workers can be replaced; Workers can migrate; Workers can restart.
The workflow survives.
1. Primary interface: ✅ Web. Everything else (CLI, TUI, desktop) becomes optional clients.
2. Human-in-the-loop: Humans should intervene at policy boundaries, not individual agent steps.
Examples:  approve deployment; approve merge to main; approve production infrastructure changes; approve external communication. 
The rest should remain autonomous.
1. Workflow Philosophy: Directed execution graphs (DAGs) with dependencies, retries, approvals, and recovery. A Kanban board can simply render that state for humans.
2. Model-agnostic.
Today:  Grok, Claude, OpenAI, Gemini, Local models etc
Tomorrow: Anything implementing the provider interface.
1. SUMMARY: Agent Fleet is a production-grade orchestration platform for long-running AI workers. It enables software teams to coordinate, supervise, and recover hundreds of autonomous coding agents working toward a shared engineering goal through durable workflows, shared context, and human-governed execution.


@Tarun Gaba’s vision:
We are building a tool that allows a fleet of agents to operate with minimal inputs and be able to work autonomously to complete a bigger task or a list of smaller tasks.
It consists of two components 

* an orchestration/management/operator service which is responsible for maintaining the lifecycle of worker processes (agent prompts, environment, initiation and deletion). It should also be able to spin up agents according to the tasks and a max number specified in settings. It should also have a configuration manager where you can tweak things like max number of agents, environment variables/secrets and common goal (global system prompt).
* A worker service, which spawns an agent, picks up a task from a list and works off it. Be able to report back to the "operator" service when the task completes, (success / failure) or if it requires more inputs. It should also be able to work independently towards the commong goal. Maybe allow some sort of "screenshotting" capabilities so that the results can be verified by a human-in-the-loop.


Features:
  - A web based UI to monitor agents lifecycle and configuration management.
  - A trello-board for organizing smaller tasks and moving them to different statuses (In dev, work in progress,Done ( succeeded, failed )
  - A central document repository where someone can write task specifications and an agent can break it into several components which can then be parallelized. A way to link the document to a particular repository where the actual task will be done and committed. 
  - The "vision" doesn't need to be limited to coding tasks, but other tasks (like outgoing marketing emails, sales pipelines, analysis etc.).
  - Uses loop engineering to perform tasks, do deployments and do testing/validation.
  - A monitoring dashboard to be able to look at all agents at a high level, what they are doing and whether they are on right track.

Considerations:
  - We can use tmux sessions to handle agent lifecycle.
  - We can use screenshot/pdf capabilities from browser to report statuses.
  - Client/server architecture with streaming (websockets).
  - Agents can share state and data via Markdown files (rich text) or a document-based store (mongoDB). Confidential data can be encrypted and remain in a database (postgres or sqlite).
  - Backend stack can be Rust, Golang and FE stack can be react based (NextJS).
  - A supporting TUI and CLI for being able to initialize tasks from command line, however the primary interface will be web based.



@Daniel Vrátil’s vision:
We are building a tool that enables developers, testers, designers, architects or product managers to delegate tasks of varying complexity to the Agent Fleet. The input is a JIRA ticket, GitHub issue or similar, the output is a PR, everything in-between is automated.
The platform should automatically handle failing CI builds and PR review feedback, so that the human developer can come to a PR that is already green and only requires the final thumbs-up/sign-off before merging.
The initial goal should be to automate away repetitive, tedious and small tasks to allow engineers to focus on the truly difficult problems. Long-term the platform should be able to assist with development and testing.
The platform should be able to provide both hosted workers as well as self-hosted workers. The hosted workers ensure low barrier to entry, the self-hosted workers make the product usable for customers who need the Fleet to be able to access their internal infrastructure.

Going more technical: the system should consist of four main components:
1) connectors - those connect to JIRA, GitHub issues, etc. and ingest new tasks for the fleet
2) analyzer - analyze the tasks, verify it's well-defined and actually doable (e.g. it doesn't ask to implement a design spec if the platform doesn't have access to Figma)
3) scheduler - schedules work
4) controller - orchestrates the infrastructure, spawns and drives the coding agent

Communication: Kafka/PubSub - ensures persistence, no message is lost when service goes down or throws an exception mid-processing
Storage: Postgres/whatever
Orchestration/Infrastructure: for the hosted workers, I would go with K8s. Since hosted K8s from cloud providers are extremely expensive, for the initial version we can equally easily go for a bunch of cheap VMs from Hetzner with Rancher/k3s; for the self-hosted workers I would look into how Anthropic's Self-hosted Sandboxes work and do something similar

@Eugene Pyvovarov’s vision:

 Agent Fleet

Same Mind map in the browser

IDEA

- Run heterogeneous agents as one managed engineering team
- What it is
 - Orchestration layer over existing agent harnesses
 - Not a new coding model or chat product
- Problem today
 - Work is scattered across disconnected agent sessions
 - No shared ownership of who is doing what
 - No reliable way to bound cost or risk
 - Teams oscillate between babysitting and unattended chaos
- Stance
 - Prefer safe 2–3× throughput over max autonomy
 - Keep human judgment where maintainability has no fast oracle
- Audience
 - First: solo power developer / tech lead
 - Then: small eng team (shared board + reviews)

PROPOSED STRUCTURE

- 1. Control plane

 - Roles / lanes
 - Named lanes: backlog · implementer · reviewer
 - Used only to route tasks to the right agent type
 - Budgets
 - Hard monthly or period cap per agent
 - Auto-pause when cap is hit
 - Gates & policy
 - Plan acceptance required before large work starts
 - Protected paths cannot ship without extra approval
 - Self-merge forbidden for medium+ risk changes
 - Audit log
 - Immutable record of claims, transitions, spend, approvals
 - Supports post-hoc “what happened?” debugging

- 2. Work plane

 - Board + states
 - Canonical lifecycle: triage → ready → running → review → done
 - Blocked as an explicit waiting state
- Task size S / M / L
 - S — single-pass, low blast radius
 - M — short plan, a few slices
 - L — multi-phase plan before coding
 - Size picks process depth (not agent skill)
 - Specs & Open Questions
 - Written intent for M/L work
 - Material unknowns stay open until a human answers
 - Open questions hold the task out of implement
 - Slices
 - Thin end-to-end increments, not layer-by-layer dumps
 - Each increment ends in something touchable (test, UI, API)
 - Comments & artifacts
 - Handoffs stay on the task thread
 - Attach PRs, logs, screenshots as first-class artifacts
 - Freeform team chat is out of scope for v1

- 3. Runtime plane

 - Dispatcher
 - Promotes ready tasks on a tick
 - Spawns the assigned agent process
 - Reclaims dead or timed-out runs
 - Claims
 - Atomic checkout so two workers never take the same task
 - TTL + heartbeat for long runs
 - Expired claim returns the task to the pool
 - Workspaces
 - Per-task isolation
 - Default for code: git worktree
 - Cost metering
 - Attribute tokens and $ to agent + task
 - Feeds budget accounting (does not define the caps)

- 4. Adapter plane

 - Adapter SPI
 - Minimal verbs: assign, cancel, status, cost, messages
 - Any runtime implementing the verbs can be hired
 - Coding harnesses
 - Primary implementers against real repos
 - v1 ships with one live harness integration
 - Generalist workers (optional)
 - Research / ops-style tasks, non-code
 - Same board rules as coding agents
 - Review assist
 - Optional automated pre-check on a PR or slice
 - Does not count as the human merge decision

- Human checkpoints

 - Before coding M/L: confirm intent and plan
 - After each non-S slice: read the diff / outcome
 - Before main/prod: approve ship
 - Anytime: pause, reassign, or stop an agent

@omchillure’s vision :
Still doing a little bit research on stuff, not a specific vision until bow. I am down to go with anything we all agree upon

@Viny Kriesang’s vision:
We are building a tool that allows dev teams to work cooperatively in projects by spawning up to 1000 agents. The tool integrates local models and cloud models using API keys. It allows the dev team to design workflows where they define the criteria of success for each step. The tool can run several workflows at the same time, each one working on a different feature. Each workflow has its own environment and copy of the worktree. Agents in the same workflow share state and data with SQLite. 
Our goal is to allow 20 devs + 1000 agents to do the work of 100 devs with regular agents. And also make local model usability get on par with cloud models by coordinating a swarm of them. 
Each workflow has an orchestrator agent that spawns the other agents. If an agent realizes its task is too complex, it requests to be replaced by a sub-orchestrator, which will split the task into smaller chucks and spawn agents to do it. This creates a recursive nested architecture, where the agent tree grows according to the complexity of the task. 
If it's proven that an agent did a poor job with a task, the workflow stops and goes back to where the poor behavior was detected. The task is handed to a sub-orchestrator who will break it down into smaller chucks and spawn agents to do the task more thoroughly. The database keeps track of which agent did each task and the nesting, so the respective orchestrator knows who is the agent to blame and therefore knows which node of the tree should be expanded with a new sub-orchestrator. After the new sub-orchestrator finishes the task, the parent orchestrator evaluates which of the consecutive tasks must be re-done and which ones can be skipped.
The UI will be Web based because companies often restrict installation of software in the work PCs. However, if we realize that we need more control over the system to better manage the agents, we can switch to a desktop app. TUI is off the table because it is too limited. 
To have multiple devs managing the same agents, there must be a split between worker app and manager app, similar to Temporal. The worker app is the one that spawns agents. It can run on the cloud on a server provided by us or be self-hosted by the dev team. It can install local models as requested by the manager app. Cloud models (Grok, Claude) are also spawned by the worker because they need to have access to the worktree.
The manager app is where dev teams create projects, create workflows, select the success criteria for each step, select which models will be use, trigger workflows, and monitor the results. The manager app puts guardrails in place like, do not deploy before testing, do not merge into main without approval from a human, etc. The guardrails are configured in the workflow. 
Proposed team division: 

* UI and UX (manager app)
* agent environment and lifecycle (worker app)
* workflow execution and agent orchestration (manager app + worker app)

https://nova.teachx.ai/trace-analysis/agent-fleet/-/tree/vinys-vision



1] Product vision : direction for the agent fleet

:question: Are we thinking for managing agents as dev team? 

1. are we aim for managing agents as dev team? 
2. which agents are we about to integrate? codex/opencode/pi/hermes/openclaw/nanobot?
3. do we define workflow? how the tasks are moving along? kanban?
4. how they agents communicate? comments in each task? chat?
5. human in the loop? where it's ok to interfere and where not.
6. who is the user?
    1. single dev
    2. team of devs
    3. non-devs



Similar projects: 

* https://paperclip.ing/ 
    * define a team of agents, with token budgets, defined roles
* https://buzz.xyz/
    * communication happens via protocol which allows crypto payments(something to think about)
* https://hermes-agent.nousresearch.com/
    * agent can spawn other agents and has kanban to follow statuses



0) Main application parts:

* Agents control
    * spinup
    * rules
    * context preparation
    * model providers (connection, budget control)
* Team control
    * team composition(agents, workflow, repo, artifacts)
    * communication(human in the loop, chats)
    * kanban(tasks crud, tasks control)
    * logs(?)
    * regular jobs


1] Picking our primary interface. React with :white_check_mark: to vote. 

1️⃣ TUI (terminal) : ratatui, fastest to ship, forces a clean daemon/client split. Non-engineers might not want to use it.
2️⃣ Web : one URL, everyone on same version, works for ops/compliance/eng (best for starting).
3️⃣ Desktop app (Rust/GPUI) : richest interaction, local file access. Install friction on locked-down laptops, per-machine updates, multi-user needs a sync layer.


2] Backend & core : 
1️⃣ pure Rust 


3] Providers
- Grok 
- Local models  
- More integrations


4]  DB : 
Where does durable state live for ~1000 concurrent agents? React to vote 👇

1️⃣ SQLite (embedded) : one file, WAL mode, ships inside the binary. Zero setup for self-hosters, atomic commits, real queries. 
2️⃣ Postgres : multi-node from day one, concurrent writers. Better for our purpose.
3️⃣ Local files (JSONL append) : what most agent CLIs do. Dead simple, human-readable. Might break with concurrent runs, deadlocks.


5] Agents spinup : 
- suggest some ideas?



Team split

We thought of moving with 2 people in a team (so we can code an review the work)   
   - suggestions are welcome, can update

1] Agents + Orchestration 
2] Backend Core
3] Frontend and integrations

