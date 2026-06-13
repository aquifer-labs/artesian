<!-- SPDX-License-Identifier: Apache-2.0 -->

# Brunnr

Brunnr is a Rust workspace for multi-agent context orchestration. It starts with a non-intrusive memory layer and grows into optional master, worker, judge orchestration with pluggable agents, pluggable memory backends, and MCP-first integration.

## Status

This repository is in bootstrap. The first working path is `memory` mode with a local files backend and an MCP server exposing `memory.find` and `memory.store`.

## Quickstart

```shell
cargo build --workspace
cargo run -p brunnr-cli -- init
cargo run -p brunnr-cli -- memory store "Brunnr keeps durable context" --tag bootstrap
cargo run -p brunnr-cli -- memory find durable
```

Run the MCP server over stdio:

```shell
cargo run -p brunnr-mcp -- --root .brunnr
```

Spawn role aliases are available in plain English and Norse form:

```shell
cargo run -p brunnr-cli -- spawn master claude-code
cargo run -p brunnr-cli -- spawn thor codex
cargo run -p brunnr-cli -- spawn tyr gemini
```

## Workspace

- `brunnr-core`: role, queue, config, and agent adapter traits.
- `mimisbrunnr`: memory trait, local files backend, RRF seam, and feature-gated Qdrant seam.
- `hvergelmir`: optional sandbox runtime seam.
- `bifrost`: future TUI crate.
- `brunnr-mcp`: MCP server for memory tools.
- `brunnr-cli`: CLI entrypoint.

## Modes

- `memory`: memory backend plus MCP tools, with no orchestration requirement.
- `orchestrate`: optional master, worker, judge role routing.
- `full`: memory, orchestration, and sandboxing.
- `advanced`: bring your own existing memory or context layout.

## License

Brunnr is licensed under Apache-2.0. Contributions must include a DCO sign-off.

## Acknowledgments

Brunnr stands on the shoulders of prior work and public ideas. Brunnr reuses ideas and APIs where appropriate, not third-party source code.

- **Andrej Karpathy — LLM Knowledge Bases** — https://x.com/karpathy/status/2039805659525644595 (the md "LLM wiki" memory idea; informs the Files backend + capture discipline).
- **Qdrant** — https://github.com/qdrant/qdrant (vector store; `QdrantBackend`).
- **TencentDB Agent Memory** — https://github.com/TencentCloud/TencentDB-Agent-Memory (L0–L3 tiering, hybrid BM25+vector RRF, node_id drill-down, sqlite-vec local-first; `SqliteVecBackend` + `TencentDBBackend`).
- **OpenAI — Codex Memories & Agent Loop** — https://developers.openai.com/codex/memories · https://openai.com/index/unrolling-the-codex-agent-loop/ (memory model + the agent loop).
- **Anthropic — Claude Code Agent Memory & Agent Loop** — https://platform.claude.com/docs/en/managed-agents/memory · https://code.claude.com/docs/en/agent-sdk/agent-loop (memory + the agent loop).

Prior art also includes OpenAI Symphony and Cursor scaling-agents.
