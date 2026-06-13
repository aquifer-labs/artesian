<!-- SPDX-License-Identifier: Apache-2.0 -->

# Modes

Brunnr is **non-intrusive**: set it up once and keep driving your agent exactly as before. You opt
into only the capabilities you want. Mode is a config choice (`brunnr init` writes `brunnr.toml`).

## `memory` (default, zero workflow change)

Exposes durable memory to your agent over MCP (`memory.find` / `memory.store`) plus a session-
start context injection. You do not change how you prompt or run your agent — you simply gain
faster, cheaper context because relevant knowledge is *retrieved* instead of re-read. No
orchestration, no sandbox. `memory.backend` selects `files` (md, zero infra), `sqlite-vec` (local
hybrid, zero infra), or feature-gated `qdrant` (shared server). See [memory.md](memory.md).

## `orchestrate` (opt-in)

Adds the optional master/worker/judge roles (Óðinn/Þórr/Týr; aliases `master`/`worker`/`judge`)
and the Þing task queue. Composable: master-judge only, one agent bound to all roles
(e.g. Codex everywhere), or the full triad. No mandatory agent loop — leave delegation off if you
don't want it. See [task-tracking.md](task-tracking.md).

## `full`

`memory` + `orchestrate` + the optional `hvergelmir` Docker sandbox for isolated workers.

## `advanced` (bring-your-own)

For power users who already have a memory layout. Point Brunnr at an **existing** markdown tree or
vector collection and it adapts and overlays without owning or rewriting your schema: it reads
your structure, serves retrieval over it, and adds Brunnr capabilities on top. You keep full
control of your data model; Brunnr meets it where it is.

---

Switching modes never requires re-architecting your project. Start in `memory` mode for the token
win; add `orchestrate` or `advanced` later if and when you want them.
