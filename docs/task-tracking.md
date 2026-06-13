<!-- SPDX-License-Identifier: Apache-2.0 -->

# Þing — Brunnr Task Tracking

> *Þing was the Norse assembly where matters were decided. Brunnr's task tracker is where work
> items are filed, claimed, and resolved.*

Task tracking is a first-class Brunnr component, not just an orchestrator internal. It is usable
**standalone** (mode `tasks`) by developers who only want a fast, agent-friendly task queue, and
the orchestrator (Urðarbrunnr) consumes it **under the hood** when running master/worker/judge.

Status legend: **[planned]** — this document is the design contract; implementation follows in a
dedicated worker phase.

---

## 1. Goals

- A race-safe task queue an agent (or a human) can drive without a server.
- Storage parity with memory: works with **markdown task files** *and* a **vector DB**, behind
  one trait — same dual-backend philosophy as Mímisbrunnr.
- Optional **external connectors** (Jira, Linear) via their MCP servers, so tasks also "land" in
  the tracker the team already uses and are visible in a familiar UI.
- Reuse the proven `.agent/`-style md queue (atomic `mv` between state directories) that already
  works in practice.

## 2. Model

A task is an `Erindi` (already a core type). It carries: `id`, `title`, `priority`, `state`,
`body` (md), `tags`, `blockers`, `created_at`, `updated_at`, and optional `external_ref`
(e.g. a Linear/Jira issue id). States form the queue (`Þing`):

```mermaid
stateDiagram-v2
  [*] --> todo
  todo --> doing: claim (single authority)
  doing --> done: judge accepts (Galdr)
  doing --> blocked: blocker / needs input
  blocked --> todo: unblocked
  doing --> todo: retry
  done --> [*]
```

`Galdr` (a completed task) is reached only through the judge gate in orchestrate mode; in
standalone `tasks` mode the human moves the task. A **single mutation authority** serializes
state transitions (the orchestrator, or the CLI lock) to prevent duplicate claims — the
anti-race lesson from Symphony.

## 3. Storage seam — `TaskStore` [planned]

Mirror the memory design exactly: a thin trait with interchangeable backends.

```mermaid
flowchart TD
  TS[trait TaskStore] --> FT["FilesTaskStore — md queue, atomic mv (todo/doing/done/blocked)"]
  TS --> VT["VectorTaskStore — tasks indexed in Mímisbrunnr for semantic dedup/search"]
  TS --> XC["ExternalConnector — Jira / Linear via their MCP"]
```

`TaskStore` operations: `create`, `claim`, `transition`, `get`, `list(filter)`, `find(query)`.

- **`FilesTaskStore`** — the default, zero-infra: tasks are md files under
  `<root>/tasks/{todo,doing,done,blocked}/`, transitions are atomic `mv` (race-safe), `list` is a
  directory scan. This is the well-known `.agent/`-style filesystem task-queue pattern.
- **`VectorTaskStore`** — indexes each task's title+body into a Mímisbrunnr collection so the
  agent can `find` semantically similar prior tasks (dedup, "have we solved this before?") and so
  task search shares the memory retrieval stack (RRF, tiers). Can wrap `FilesTaskStore` as the
  source of truth and keep the vector index as a derived view.
- **`ExternalConnector`** — bridges to Jira/Linear through their MCP servers. Tasks created in
  Brunnr are mirrored to the external tracker (and optionally pulled back), giving teams a visual
  board while the agent works the local queue. The connector is opt-in and never required for the
  local modes.

## 4. How the pieces compose

```mermaid
flowchart LR
  subgraph standalone["mode = tasks (standalone)"]
    CLI[brunnr task ...] --> TSx[TaskStore]
  end
  subgraph orchestrate["mode = orchestrate"]
    UR[Urðarbrunnr orchestrator] --> TSy[TaskStore]
    UR --> OD[Óðinn/master]
    UR --> TH[Þórr/worker]
    UR --> TY[Týr/judge]
  end
  TSx -. same trait .- TSy
  TSy -. mirror .-> EXT[(Jira / Linear via MCP)]
  TSy -. index .-> MEM[(Mímisbrunnr)]
```

Standalone users get a useful queue with one binary; orchestrate users get the same queue driven
by the agent loop, with the judge as the sole gate to `done`. Because `TaskStore` is one trait,
md-only teams, vector-indexed teams, and Jira/Linear teams all use the same commands.

## 5. CLI [planned]

```bash
brunnr task add "Refactor vector layer" --priority p1
brunnr task list --state todo
brunnr task claim <id>            # todo -> doing (single authority)
brunnr task done <id>             # doing -> done (judge gate in orchestrate mode)
brunnr task find "vector backend" # semantic search over tasks (VectorTaskStore)
brunnr task connect linear        # enable the Linear MCP mirror
```

## 6. Decisions

- Task tracking is a **separate component** (`crates/thingr` [planned]) so it can be used on its
  own, and is **composed** by the orchestrator rather than duplicated inside it.
- Storage parity with memory via a `TaskStore` trait; `FilesTaskStore` is the zero-infra default;
  `VectorTaskStore` reuses Mímisbrunnr; external trackers are connectors, never a hard dependency.
- Single mutation authority + atomic `mv` keep the queue race-free without a database.
