<!-- SPDX-License-Identifier: Apache-2.0 -->

# Agent Teams — Flume

A **Flume** (a cluster of wells drawing from one shared aquifer) is a vendor-neutral agent team: a **lead** plus
**teammates** (and an optional **judge**) that coordinate over a shared task board and a shared
message pool, all reading and writing **one shared persistent memory**. Teammates can be backed by
any agent and model — Claude, Codex, opencode, Gemini, a local model — and every teammate runs
**supervised**, so a team never leaks or orphans processes.

Flume is not a separate product or a rewrite. It composes primitives Artesian already has: the headrace
task board, the EventEnvelope message pool, the Basin role loop, supervised process spawning,
and Aquifer memory.

![Flume team architecture](diagrams/flume-team.png)

## Where it fits: a topology, not a new mode

Teams are a **topology of `orchestrate` / `full`**, not a fifth [mode](modes.md). A Flume is simply
`orchestrate` scaled from a single worker to several coordinating teammates — you opt in exactly as
you opt into orchestration, and `memory` mode is unaffected.

| Mode | Memory | Orchestration | Team topology |
|---|---|---|---|
| `memory` | yes | — | — |
| `orchestrate` | yes | master / worker / judge | single worker **or** a Flume |
| `full` | yes | + sandbox | single worker **or** a Flume |
| `advanced` | your store | bring-your-own | Artesian coordinates your agents |

## Roles: three archetypes, your own names

Artesian keeps a small, clear coordination grammar — three **archetypes** (kinds):

| Kind | Responsibility |
|---|---|
| `master` | the lead: plans, splits work, assigns/synthesizes |
| `worker` | a teammate that executes a bounded task |
| `judge` | verifies a result before it is accepted (sole committer) |

On top of that, **you define your own named specializations** — `security-reviewer`, `architect`,
`test-runner` — each mapping to one archetype. So you name roles in your own language while the
system keeps a predictable master/worker/judge structure underneath.

## Defining teammates — `.agent/agents/*.md`

Teammate roles are plain, vendor-neutral, git-versioned files — human-editable and diffable:

```markdown
---
name: security-reviewer
kind: worker                    # master | worker | judge
description: Reviews auth code for vulnerabilities.   # used for routing/selection
agent: codex                    # optional; else resolved from bindings/catalog
model: gpt-5.5                  # optional; a specific model
allow_tools: [read, grep, memory.find]   # optional tool scoping
---
You are a security reviewer. Focus on token handling, session management, and input
validation. Report findings with severity ratings. (appended to the teammate's system prompt)
```

- **Definitions are configuration, so they live in files** — version-controllable and shareable in
  the repo, not in the vector store. (Memory *records* and their tenancy metadata do live in the
  vector store payload, as usual — see [memory.md](memory.md). Definitions are not search objects.)
- Artesian also **reads existing `.claude/agents/*.md`** definitions, so you can reuse roles you have
  already written instead of redefining them. Interop reads `name`, `description`, `tools`,
  `model`, and the body prompt addendum; if no Artesian `kind` is present, Artesian infers the
  archetype from the name (`lead`/`master` -> `master`, `review`/`judge` -> `judge`, otherwise
  `worker`). Artesian does not copy vendor-reserved schema semantics.
- Optional: definitions can additionally be indexed for semantic "find a role that does X"
  discovery; the files remain the source of truth.
- `agents.list` includes both reachable agent/model entries and these role-definition summaries.
  Before spawn, a definition's resolved agent/model is validated against the catalog; unavailable
  bindings fail before any process starts.

## How a team works

- **Shared task board (headrace):** the lead creates tasks with dependencies (a DAG); teammates
  self-claim the next unblocked task (atomic file-lock claim) or the lead assigns explicitly;
  completing a task unblocks its dependents automatically. See [task-tracking.md](task-tracking.md).
- **Shared message pool / blackboard (EventEnvelope, publish-subscribe):** teammates post and read
  typed messages (`ASK`, `RESULT`, `REVIEW`, `DONE`); direct teammate-to-teammate addressing rides
  on the same pool.
- **Verifier + judge:** a result passes a verifier (the trust boundary) and an optional judge before
  it is accepted; the judge is the sole committer.
- **Optional plan approval:** teams or specific role definitions can require a pre-execution plan.
  In that case a task cannot be claimed until a `REVIEW` message with approval is posted by the
  judge or lead. This is off by default because reviewers can reject good work as well as catch bad
  plans.
- **Feedback-aware admission seam:** teams bound the number of admitted teammates. When the cap is
  reached, a teammate is paused instead of killed; current implementation reuses spawn caps and
  quotas, leaving room for adaptive AIMD-style admission later.
- **Lifecycle & safety:** every teammate spawns through the supervised process layer (own process
  group, persistent registry, startup reaper), so a teammate — and any child it spawns — is always
  cleaned up on completion, timeout, cancellation, or a crash. A team cannot fill the machine with
  orphans. See [concurrency.md](concurrency.md).

## Memory is the substrate (and the flagship)

Every teammate reads and writes **one shared persistent memory** (Aquifer), scoped by tenancy
(`team` / `agent` / `task` / `session`). So teammates share a working context **and** that context
survives across sessions and context compaction — the gap in current team systems, where teammates
do not inherit each other's history and nothing persists between runs. `memory.context` returns each
teammate just the slice it needs, which is where the token saving comes from
([benchmark](../benchmarks/README.md)).

## Integration depths — use as little or as much as you want

1. **Memory only (the flagship):** point your **existing** team / sub-agent / orchestrator system at
   Artesian's memory over MCP (`memory.find` / `memory.context` / `memory.store`). You add persistent,
   token-saving shared memory to whatever you already run — Claude agent-teams, a custom
   orchestrator, LangGraph — with no orchestration takeover.
2. **Coordination (`advanced`):** keep your own agents; use Artesian's shared task board and message
   pool as the coordination substrate.
3. **Full Flume:** Artesian runs the team end to end — vendor-neutral, verifier-gated, supervised.

Interop rule — **do not double-orchestrate.** When a native team system (e.g. Claude Code agent
teams) is driving the loop, run Artesian in `memory` / `advanced`, not `orchestrate`. Artesian's
orchestration tools are off outside `orchestrate` / `full`, so this is the default.

## Surfaces

- **MCP team tools:** `team.create`, `team.spawn`, `team.task.add` / `claim` / `complete`,
  `team.message`, `team.status`, `team.presence`, `team.lane.add`, `team.lane.assign`,
  `team.cleanup` — over the same supervised engine.
- **CLI:** `artesian team …` exposes the same operations for foreground/local use, including
  `artesian team presence <team-id>` for a live snapshot.
- **Lead role-skill:** a short instruction `artesian init` writes so an in-session lead drives the team
  natively (read the catalog, recall via `memory.context`, delegate, gate via the judge).

## Driving teams and the agentic loop through MCP

An agent (e.g. Claude in an MCP session) can drive **both** multi-agent spawn/management **and** the
full agentic loop entirely through the Artesian MCP server, without a separate CLI invocation.

### Full MCP tool catalog (orchestrate / full mode)

| Tool | Purpose |
|---|---|
| `agents.list` | List reachable agent CLIs and available models, plus role-definition summaries |
| `orchestrate.bind` | Bind a role to an agent/model for this MCP session |
| `orchestrate.delegate` | Dispatch one bounded task to the supervised role agent (single-shot) |
| `orchestrate.loop` | Run the full agentic loop: recall → worker → verify → commit (see below) |
| `orchestrate.status` | Check status and result of a delegated task |
| `orchestrate.handoff` | Record a handoff to judge or master |
| `team.create` | Create a Flume multi-agent team topology |
| `team.spawn` | Admit and spawn a teammate from a `.agent/agents` or `.claude/agents` definition |
| `team.task.add` | Add a task to the shared headrace task board |
| `team.task.claim` | Atomically claim an eligible task, respecting plan approval gates |
| `team.task.complete` | Complete or block a task through the judge/master gate |
| `team.message` | Post typed messages (ASK / RESULT / REVIEW / DONE) to the EventEnvelope pool |
| `team.status` | Inspect team lifecycle, teammates, and redacted message pool |
| `team.presence` | Live presence snapshot: active lanes/tasks/owners/load vs. cap |
| `team.lane.add` | Register a specialist lane with a written contract (scope, non-goals, budget, handoff) |
| `team.lane.assign` | Assign a task to a lane, enforcing dedup and budget |
| `team.cleanup` | Terminate tracked teammate process groups and mark the team cleaned up |
| `team.gc` | Garbage-collect orphaned, expired, or hung teammate process groups |

### `orchestrate.loop` — agentic loop over MCP

`orchestrate.loop` runs the same implementation as the CLI `artesian loop` command: each turn the
loop recalls goal-relevant memory, assembles a bounded goal packet (goal + invariants + last-failed
check + MMR-diversified recall), runs the worker with that packet injected via `ARTESIAN_PACKET` /
`ARTESIAN_GOAL` / `ARTESIAN_RECALL` / `ARTESIAN_TURN` env vars, writes a resume anchor, verifies the
goal, and on success stores a verified skill + spec + auto-invariants into memory. The same brakes
apply: `max_turns` (default 10), `max_wall_secs`, and the `~/.artesian/STOP` sentinel file.

Parameters: `goal` (verifier command), `worker` (per-turn action), `max_turns?`, `max_wall_secs?`,
`no_learn?` (disable skill/spec/invariant storage). Returns: `outcome`, `why_stopped`, `turns`,
`run_log_path`.

#### Example: MCP-driven agentic loop

```json
{
  "tool": "orchestrate.loop",
  "params": {
    "goal": "cargo test --workspace 2>&1 | tail -1 | grep -q 'test result: ok'",
    "worker": "claude -p \"$ARTESIAN_PACKET\" --permission-mode acceptEdits",
    "max_turns": 8,
    "max_wall_secs": 600
  }
}
```

The outcome report:

```json
{
  "outcome": "success",
  "why_stopped": "goal held",
  "turns": 3,
  "run_log_path": "/Users/you/.artesian/runs/loop-1750000000000.jsonl"
}
```

#### Example: MCP-driven Flume team

```json
// 1. Create team
{ "tool": "team.create", "params": { "name": "patch-team", "plan_approval_required": true } }

// 2. Spawn teammates from definitions
{ "tool": "team.spawn", "params": { "team_id": "patch-team", "definition": "security-reviewer" } }
{ "tool": "team.spawn", "params": { "team_id": "patch-team", "definition": "judge-a" } }

// 3. Add a task
{ "tool": "team.task.add", "params": { "team_id": "patch-team", "title": "Review auth.rs", "definition": "security-reviewer" } }

// 4. Judge approves plan
{ "tool": "team.message", "params": { "team_id": "patch-team", "from": "judge-a", "to": "security-reviewer", "kind": "review", "content": "Plan approved", "task_id": "<id>", "approved": true, "execute": false } }

// 5. Worker claims and executes
{ "tool": "team.task.claim", "params": { "team_id": "patch-team", "task_id": "<id>", "teammate": "security-reviewer" } }
{ "tool": "team.message", "params": { "team_id": "patch-team", "from": "security-reviewer", "to": "security-reviewer", "kind": "ask", "content": "Review auth.rs for injection risks", "execute": true } }
```

**Interop rule — do not double-orchestrate.** When a native team system (e.g. Claude Code's own
agent-teams) is driving the loop, keep Artesian in `memory` or `advanced` mode so its orchestration
tools stay off. The `team.*` and `orchestrate.*` tools are disabled outside `orchestrate` / `full`
mode by design.

## Parallel specialist lanes

**Openclaw's key lesson:** parallelism in multi-agent systems is a *scarce-resource* problem — not
"more agents = more throughput". The bottlenecks are session locks, model capacity, tool
availability, context limits, and especially **ownership ambiguity**: two agents picking up the same
task wastes both turns and produces conflicting state.

Flume addresses this with **lane contracts**.

### Lane / LaneContract

A `Lane` is a named specialist with a written contract that declares exactly:

| Field | Meaning |
|---|---|
| `name` | Stable lane identifier (`"security"`, `"test-runner"`) |
| `definition` | Role definition file this lane uses |
| `owned_scope` | Human-readable description of what this lane owns |
| `non_goals` | What it must NOT do (prevents scope overlap with sibling lanes) |
| `budget.max_concurrent_tasks` | Max tasks active simultaneously (within global cap) |
| `budget.max_turns` | Max total worker invocations across all tasks |
| `budget.token_cap` | Advisory soft cap (recorded, not enforced) |
| `handoff_to` | Lane or teammate to receive handoff summaries on completion |
| `allowed_tools` | Tools this lane may call (empty = no additional restriction) |
| `agent_constraint` | Agent CLI this lane uses (optional) |

Lanes run in parallel under the **existing global `max_concurrent_spawns` cap** — the cap is never
increased, just made legible across specialist domains.

### Coordinator deduplication

The `LaneCoordinator` lives inside `TeamRuntime` and enforces two properties:

1. **Dedup** — the same task (matched by id OR canonical title) cannot be active in two lanes
   simultaneously. A second assignment to the same task is rejected with a clear error.
2. **Budget** — each lane's concurrent-task cap is enforced before assignment.

The coordinator is small and synchronous — no async, no I/O. Coordination state is handled by the
surrounding `TeamRuntime` mutex, the same lock that already serialises team mutation.

### MCP tools

```json
// Register a specialist lane
{
  "tool": "team.lane.add",
  "params": {
    "team_id": "patch-team",
    "name": "security",
    "definition": "security-reviewer",
    "owned_scope": "Authentication, token handling, session management",
    "non_goals": ["UI testing", "performance benchmarks"],
    "max_concurrent_tasks": 2,
    "max_turns": 20,
    "handoff_to": "judge"
  }
}

// Assign a task to a lane (dedup enforced)
{
  "tool": "team.lane.assign",
  "params": {
    "team_id": "patch-team",
    "lane_name": "security",
    "task_id": "task-42",
    "task_title": "Review auth.rs"
  }
}
```

### CLI

```
artesian team lane add  <team-id> --name security --definition security-reviewer \
  --owned-scope "auth and token handling" --max-concurrent-tasks 2
artesian team lane assign <team-id> --lane-name security --task-id task-42 --task-title "Review auth.rs"
```

---

## Presence — live state visibility

Before spawning or assigning work, an orchestrator (or a human operator) needs to see what is
already active to avoid stepping on an in-flight lane.  `team.presence` returns a live snapshot:

```json
{
  "tool": "team.presence",
  "params": { "team_id": "patch-team" }
}
```

Response:

```json
{
  "presence": {
    "team_id": "patch-team",
    "lanes": [
      {
        "name": "security",
        "definition": "security-reviewer",
        "active_task_ids": ["task-42"],
        "turns_used": 3,
        "tokens_used": 0
      }
    ],
    "teammates": [
      {
        "name": "security-reviewer",
        "lane": "security",
        "status": "active",
        "active_task_ids": ["task-42"]
      }
    ],
    "total_active_tasks": 1,
    "spawns_active": 1,
    "spawns_cap": 4
  }
}
```

Presence reuses the existing heartbeat/registry infrastructure — it is derived from in-memory state
with no additional I/O overhead.  The CLI equivalent: `artesian team presence <team-id>`.

---

## Runtime trait — the model-loop seam

The per-agent invocation shape (claude / codex / gemini / opencode / generic) is now also available
through a clean `AgentRuntime` trait in `artesian-process-agent`:

```rust
pub trait AgentRuntime: Send + Sync {
    fn agent_id(&self) -> &str;
    fn spawn_session(&self, request: SpawnRequest) -> BoxFuture<AgentResult<AgentSession>>;
    fn send_message(
        &self, session: &AgentSession,
        message: AgentMessage,
        event_sender: Option<UnboundedSender<WorkerEvent>>,
    ) -> BoxFuture<AgentResult<String>>;
}
```

**Contract:**
- `spawn_session` is cheap: allocates bookkeeping, no process launched.
- `send_message` is the heavy path: spawns the process, pipes the prompt, streams events,
  returns the response.
- The per-agent argv (claude: `-p … --output-format stream-json --verbose --permission-mode
  acceptEdits`, codex: `exec --json --dangerously-bypass-approvals-and-sandbox`, etc.) is
  **byte-identical** to the existing `build_invocation` path — nothing changes for existing
  callers.

`ProcessAgentRuntime` is the built-in impl.  Adding a new runtime (API-based, mock for tests,
alternative model provider) is a small `impl AgentRuntime`:

```rust
let runtime: Box<dyn AgentRuntime> = Box::new(ProcessAgentRuntime::from_config(config));
let session = runtime.spawn_session(SpawnRequest { role: Role::Worker, .. }).await?;
let content = runtime.send_message(&session, AgentMessage { content: prompt }, None).await?;
```

The trait boundary is stable and clean enough that `flume` could be extracted as a standalone
product without changing callers: the `AgentRuntime` contract is the only seam that matters.

---

## End-to-end example: lanes + presence + loop

```json
// 1. Create team
{ "tool": "team.create", "params": { "name": "patch-team" } }

// 2. Register specialist lanes with contracts
{
  "tool": "team.lane.add",
  "params": {
    "team_id": "patch-team", "name": "security", "definition": "security-reviewer",
    "owned_scope": "auth, tokens, session", "non_goals": ["ui", "perf"],
    "max_concurrent_tasks": 1, "handoff_to": "judge"
  }
}
{
  "tool": "team.lane.add",
  "params": {
    "team_id": "patch-team", "name": "test-runner", "definition": "test-writer",
    "owned_scope": "unit and integration tests", "non_goals": ["security review"],
    "max_concurrent_tasks": 2
  }
}

// 3. Check presence before assigning (see what's already active)
{ "tool": "team.presence", "params": { "team_id": "patch-team" } }

// 4. Add a task and assign it to the security lane (dedup enforced)
{ "tool": "team.task.add", "params": { "team_id": "patch-team", "title": "Review auth.rs" } }
{
  "tool": "team.lane.assign",
  "params": {
    "team_id": "patch-team", "lane_name": "security",
    "task_id": "<id>", "task_title": "Review auth.rs"
  }
}
// Attempting to assign the same task to test-runner returns LaneDuplicate error.

// 5. Spawn the lane worker and run the loop
{ "tool": "team.spawn", "params": { "team_id": "patch-team", "definition": "security-reviewer" } }
{
  "tool": "orchestrate.loop",
  "params": {
    "goal": "cargo clippy --workspace -- -D warnings 2>&1 | grep -c error | grep -q '^0$'",
    "worker": "claude -p \"$ARTESIAN_PACKET\" --permission-mode acceptEdits",
    "max_turns": 6
  }
}
```

---

## Prior art and naming

Flume builds on established multi-agent patterns — MetaGPT's role-based publish-subscribe message
pool, OpenAI Symphony's single-authority dispatch, agent-teams-ai, and Claude Code's agent teams and
sub-agents. Artesian reuses ideas and credits them; it does not reproduce their code, specifications,
or marks. The `.agent/agents/*.md` schema and the hydro naming are Artesian's own.
