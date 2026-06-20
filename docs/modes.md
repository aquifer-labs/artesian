<!-- SPDX-License-Identifier: Apache-2.0 -->

# Modes

Artesian is **non-intrusive**: set it up once and keep driving your agent exactly as before. You opt
into only the capabilities you want. Mode is a config choice (`artesian init` writes `artesian.toml`).

## `memory` (default, zero workflow change)

Exposes durable memory to your agent over MCP (`memory.find` / `memory.store`) plus a session-
start context injection. You do not change how you prompt or run your agent — you simply gain
faster, cheaper context because relevant knowledge is *retrieved* instead of re-read. No
orchestration, no sandbox. `memory.backend` selects `files` (Open Knowledge Format md bundle, zero
infra), `sqlite-vec` (local hybrid, zero infra), or feature-gated `qdrant` (shared server). See
[memory.md](memory.md).

## `orchestrate` (opt-in)

Adds the optional master/worker/judge roles and the headrace task queue. Composable: master-judge only, one agent bound to all roles
(e.g. Codex everywhere), or the full triad. No mandatory agent loop — leave delegation off if you
don't want it. See [task-tracking.md](task-tracking.md).

## `full`

`memory` + `orchestrate` + the optional `sandbox` Docker sandbox for isolated workers.

## `advanced` (bring-your-own)

For power users who already have a memory layout. Point Artesian at an **existing** markdown tree
(including any **OKF** bundle) or vector collection and it adapts and overlays without owning or
rewriting your schema: it reads your structure, serves retrieval over it, and adds Artesian
capabilities on top. You keep full control of your data model; Artesian meets it where it is.

## Loop memory kit (portable across agents)

The **loop memory kit** is the anchor-set bundle that makes Artesian portable across Codex and
Claude Code (or any MCP-compatible agent). It lives in `.artesian/kit/` and contains:

- `vision.md` — project purpose, goals, current phase.
- `agents.md` — agent roster, roles, capabilities.
- `kit/index.md` — manifest + session-start recipe.

```sh
# Create the kit (once per project):
artesian kit init --vision "Build a memory control plane for agent loops"

# At session start (any agent, any model):
artesian memory anchor recover    # restore last anchor + targeted recall
artesian kit status               # print vision + anchor summary

# Export the full kit as a single markdown bundle:
artesian kit export               # stdout
artesian kit export --output kit-bundle.md
```

Over MCP the same kit is available as:
- `memory.kit.get` — returns vision, agents.md, and last anchor in one call (load at session start).
- `memory.kit.set` — updates the vision (call when the phase changes).

The kit works identically in Codex and Claude Code. Swap the model; keep the kit. This is the
"own your learning loop" principle: the company-veteran context travels with the project, not
with the agent.

---

Switching modes never requires re-architecting your project. Start in `memory` mode for the token
win; add `orchestrate` or `advanced` later if and when you want them.
