<!-- SPDX-License-Identifier: Apache-2.0 -->

# Onboarding

Two ways to bring Brunnr up: **a human follows the Quickstart**, or **an AI agent follows the
agent recipe** below. Both are non-destructive and idempotent — running them twice changes
nothing the second time, and they never delete existing memory or overwrite unrelated config.

Brunnr runs with **zero configuration** in `memory` mode on the Files (OKF) backend. Add a vector
backend or orchestration only when you want them. Sensible defaults everywhere.

---

## A. Human path (Quickstart)

```shell
cargo build --workspace

# memory mode, zero-infra Files (OKF) backend — the default
cargo run -p brunnr-cli -- init
cargo run -p brunnr-cli -- memory store "Brunnr keeps durable context" --tag bootstrap
cargo run -p brunnr-cli -- memory find durable
```

Pick a backend (config choice, not a code change):

```shell
cargo run -p brunnr-cli -- init --backend sqlite-vec          # local hybrid, zero infra
cargo run -p brunnr-cli -- init --backend qdrant \            # shared / multi-user
  --qdrant-url http://HOST:6333 --qdrant-rest-url http://HOST:6333
```

`brunnr init` detects installed agent CLIs and writes the MCP registration for each (Claude Code,
Codex, Zed) pointing at `brunnr-mcp` with the pinned embedding model and behavior-guiding tool
descriptions. Then drive your agent exactly as before — it now has `memory.find` / `memory.store`.

Backfill existing notes (idempotent), and explore modes:

```shell
cargo run -p brunnr-cli -- backfill ./memory-export   # md/json -> OKF, content-hash dedup
```

See [modes.md](modes.md), [memory.md](memory.md), [concurrency.md](concurrency.md),
[upgrades.md](upgrades.md).

---

## B. AI-agent path (recipe any agent can follow)

Any agent (Codex, Claude Code, Gemini CLI, opencode, …) can bring Brunnr up from these minimal,
deterministic steps. Ask the human operator for the four inputs, then execute — **idempotently and
without breaking anything**.

**Inputs to collect from the operator:**
- `mode`: `memory` (default) | `orchestrate` | `full` | `advanced`
- `backend`: `files` (default) | `sqlite-vec` | `qdrant`
- `qdrant_url` / `qdrant_rest_url` / API key — only if `backend = qdrant`
- `project`: the project name (becomes the collection / OKF bundle scope) and the path to any
  existing memory to backfill

**Steps:**
1. Build or locate the binary: `cargo build --workspace` (or use a prebuilt `brunnr`).
2. `brunnr init --mode <mode> --backend <backend> [--qdrant-url … --qdrant-rest-url …]`. This is
   idempotent and only writes Brunnr's own MCP entry; it must NOT touch unrelated config.
3. If `backend = qdrant`: verify the server is reachable (`/healthz`) and that the collection's
   compat metadata (model + dim) matches the pinned model; if it mismatches, STOP and ask — run
   `brunnr migrate` rather than mixing vector spaces.
4. Backfill the project's existing memory into the OKF bundle: `brunnr backfill <path>`
   (idempotent, content-hash dedup; never deletes the originals).
5. Verify: `brunnr memory store "<probe>"` then `brunnr memory find "<probe>"` returns it; report
   the backend, collection, and counts back to the operator.
6. Report what changed (config entries added, records backfilled) and what was left untouched.

**Hard guardrails for the agent (do NOT violate):**
- Never delete or overwrite existing memory or unrelated MCP/config; `init` and `backfill` are
  additive/idempotent.
- Keep secrets (API keys) out of git; store them where the operator specifies.
- Do not change the pinned embedding model for an existing collection — that needs `migrate`
  (rebuild from OKF), not an in-place switch.
- `orchestrate`/`full` only when the operator asked for it; `memory` mode must not change how the
  operator already drives the agent.
- Do not `git push` or perform outward-facing actions without explicit operator approval.

This recipe is the canonical bring-up; `AGENTS.md` points here so any agent picks it up.
