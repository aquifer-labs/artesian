<!-- SPDX-License-Identifier: Apache-2.0 -->

# Artesian

**Memory control plane for agent loops.** Long-running agent loops fail not because the model is
weak — they fail because the agent **forgets**. Artesian is the memory control plane that keeps the
working context small, high-signal, and survivable across compaction and disconnects, so memory
**guides the next action** (not just recalls a fact), **compounds** across runs, and is **owned by
you** — portable across any model and any retrieval store.

**Recall ≠ use.** Systems that saturate recall benchmarks still fail when memory must guide action:
not *can you recall attempt 12*, but *given attempts 1–46, what do you do on 47?* (MemoryArena,
arXiv:2602.16313). Artesian is the first OSS memory system that benchmarks both: recall quality
(LoCoMo / LongMemEval) and memory-guides-action (agentic task eval — see
[benchmarks](benchmarks/README.md)).

**Own your learning loop.** Swap the model, keep the company veteran. Artesian keeps durable
knowledge in portable [Open Knowledge Format](docs/backends.md) markdown you can read, edit, and
`git` — white-box, yours. Not a cloud service you rent, not a black box you can't inspect. Run
locally with zero infrastructure, no per-write LLM call, nothing leaving your machine.

We are **not** "another memory store" — we are the control plane that sits over one. The
[Agent Cognitive Compressor](docs/positioning.md) qualify-gate and bounded committed state are the
trust boundary between raw recall and what the agent actually acts on. Architecture is
**interface ≠ substrate**: human-readable OKF files as the interface, a transactional,
semantically-indexed, multi-writer substrate underneath — in one Rust binary.

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
&nbsp;status: bootstrap

[![System map](docs/diagrams/system-map.png)](docs/diagrams/system-map.mmd)

On our [retrieval benchmark](benchmarks/README.md), Artesian holds per-query context cost at
**~1,000 tokens** while the memory grows past a million tokens — a realistic
multi-session history is already 100k+ tokens of accumulated reasoning, tool output, and messages.
Full-context replay grows with the history, so the saving rises from **93% to 99.9%** as memory
scales (13k → 1M tokens). See the [chart, tables, and methodology](benchmarks/README.md).

**What you get**

- **Memory you own** — durable knowledge stored as portable [Open Knowledge Format](docs/backends.md)
  markdown: white-box, git-versionable, yours. Not a black box, not a service you rent.
- **Free, private writes** — capture is explicit and local; no per-write LLM call and nothing leaves
  your machine (LLM consolidation is opt-in). Run with zero infrastructure (Files / sqlite-vec) or a
  shared server (Qdrant).
- **Targeted recall, not replay** — ~1,000 tokens per query no matter how large the memory grows
  (the saving above). Records are chunked on store, so recall returns the few relevant *chunks*
  and stays bounded even when a single document is huge — the full source is one `node_id`
  drill-down away.
- **Plugs into anything** — MCP-first: run memory-only and add persistent context to *any* agent or
  orchestrator (Claude Code, Codex, agent teams, your own loop) with no takeover and no lock-in.
- **More than memory, when you want it** — optional master/worker/judge orchestration and
  vendor-neutral [agent teams (Flotilla)](docs/teams.md), as composable components you opt into.

**New here?** [docs/onboarding.md](docs/onboarding.md) has a human Quickstart **and** an idempotent
AI-agent bring-up recipe (any agent — Codex, Claude Code, Gemini CLI, opencode — can deploy Artesian
per your config and connect a project, non-destructively). Why Artesian vs other memory tools:
[docs/positioning.md](docs/positioning.md).

## Status

This repository is in bootstrap. The working path is `memory` mode with local Files or SqliteVec backends, optional Qdrant integration, and an MCP server exposing `memory.find` and `memory.store`.

## Install

From crates source (installs the `artesian` CLI and the `artesiand` daemon):

```shell
cargo install --git https://github.com/aquifer-labs/artesian artesian-cli
```

Then:

```shell
artesian init                                   # detect agents, write config + MCP registration
artesian memory store "Artesian keeps durable context" --tag bootstrap
artesian memory find durable
```

Signed release binaries, an `install.sh`, and a container image are planned for the first tagged
release (see [Status](#status)).

## Quickstart (from source)

```shell
cargo build --workspace
cargo run -p artesian-cli -- init
cargo run -p artesian-cli -- memory store "Artesian keeps durable context" --tag bootstrap
cargo run -p artesian-cli -- memory find durable
```

Initialize with the zero-infrastructure vector backend:

```shell
cargo run -p artesian-cli -- init --backend sqlite-vec
```

Run the MCP server over stdio using the generated config:

```shell
cargo run -p artesian-mcp -- --config artesian.toml
```

Backfill markdown or JSON memories idempotently:

```shell
cargo run -p artesian-cli -- backfill ./memory-export
```

Spawn a role-bound agent with the canonical role name (`master` / `worker` / `judge`):

```shell
cargo run -p artesian-cli -- spawn master claude-code
cargo run -p artesian-cli -- spawn worker codex
cargo run -p artesian-cli -- spawn judge gemini
```

## Workspace

- `artesian-core`: role, queue, config, and agent adapter traits.
- `aquifer`: memory trait, Files backend, generic vector memory backend, SqliteVec vector store, RRF seam, and feature-gated Qdrant vector store.
- `sandbox`: optional sandbox runtime seam.
- `gauge`: future TUI crate.
- `artesian-mcp`: MCP server for memory tools.
- `artesian-cli`: CLI entrypoint.
- `artesian-test-support`: shared helpers for crate-level integration tests.

## Modes

- `memory`: memory backend plus MCP tools, with no orchestration requirement.
- `orchestrate`: optional master, worker, judge role routing.
- `full`: memory, orchestration, and sandboxing.
- `advanced`: bring your own existing memory or context layout.

## Composes with

Artesian is the **control plane** for agent memory — it decides what enters bounded state, when to
evict, and how to survive compaction. It is designed to compose with complementary layers rather than
absorb them:

| Layer | What it does | How it composes |
|---|---|---|
| **headroom** | Data-plane compression — shrinks artifact bytes before a fact is qualified | Enable the `headroom` feature in `headgate`; wrap any `Compressor` with `HeadroomCompressor`. headroom runs under the ACC gate; Artesian still controls admission. |
| **Ollama / LM Studio / mlx_lm.server** | Local LLM inference — zero token cost, private | Named as `provider: "ollama"` / `"lm-studio"` / `"mlx"` in `AccLlmConfig`. Drop-in for the judge and compressor. |
| **Any OpenAI-compatible endpoint** | Hosted or self-hosted LLM | `provider: "openai-compatible"` + `base_url`. |
| **Any agent CLI** | Codex / Claude Code / Gemini / opencode | `provider: "command"` — shelled out via stdin/stdout. |

The principle: Artesian owns the *control plane* (what, when, why); you choose the *data plane*
(how much to compress, which model scores, which store retrieves).

## License

Artesian is licensed under Apache-2.0. Contributions must include a DCO sign-off.

## Development

Artesian uses crate-level integration tests, shared test helpers, and repo-level tooling modeled on
mature Rust workspaces. See [docs/development.md](docs/development.md).

The public memory benchmark is in [benchmarks/README.md](benchmarks/README.md) and runs with
`just bench`.

## Acknowledgments

Artesian stands on the shoulders of prior work and public ideas. Artesian reuses ideas and APIs where appropriate, not third-party source code.

- **Andrej Karpathy — the "LLM wiki" pattern** — https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f (announced at https://x.com/karpathy/status/2039805659525644595) — the LLM-maintained markdown knowledge base (`index.md` catalog + `log.md` + entity/concept pages; ingest/query/lint) that directly informs Artesian's Files/OKF backend and its consolidation roadmap.
- **Qdrant** — https://github.com/qdrant/qdrant (vector store; `QdrantBackend` via `QdrantVectorStore`).
- **TencentDB Agent Memory** — https://github.com/TencentCloud/TencentDB-Agent-Memory (L0–L3 tiering, hybrid BM25+vector RRF, node_id drill-down, sqlite-vec local-first; `SqliteVecBackend` + `TencentDBBackend`).
- **OpenAI — Codex Memories & Agent Loop** — https://developers.openai.com/codex/memories · https://openai.com/index/unrolling-the-codex-agent-loop/ (memory model + the agent loop).
- **Anthropic — Claude Code Agent Memory & Agent Loop** — https://platform.claude.com/docs/en/managed-agents/memory · https://code.claude.com/docs/en/agent-sdk/agent-loop (memory + the agent loop).
- **Open Knowledge Format (OKF)** — Google Cloud `knowledge-catalog` (Apache-2.0) — https://github.com/GoogleCloudPlatform/knowledge-catalog — the portable markdown+YAML knowledge-bundle format Artesian's `files` memory backend aligns with.
- **ApX Machine Learning — Agentic LLM Systems & Memory Architectures** (course) — https://apxml.com/courses/agentic-llm-memory-architectures — an educational reference that helped frame our memory, retrieval, consolidation, planning, and multi-agent design. The underlying techniques are grounded in their **primary sources** (cited per-topic in `docs/`); ApX content is credited and linked, never reproduced.

- **h5i** — https://github.com/h5i-dev/h5i — an AI-aware Git sidecar (per-commit agent context,
  inter-agent messaging with union-merge, and progressive sandbox isolation). A complementary layer
  to Artesian's semantic memory; its typed agent-handoff messaging informs Artesian's orchestration
  handoff protocol, and its isolation tiers inform the `sandbox` sandbox direction.

Prior art also includes OpenAI Symphony, Cursor scaling-agents, and the self-correcting /
long-running coding-agent patterns (e.g. "Autonomous Long-Running Coding Agents"). All
acknowledgments are references to public ideas/APIs and link to the sources; no third-party
content is reproduced.
