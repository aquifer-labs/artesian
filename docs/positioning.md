<!-- SPDX-License-Identifier: Apache-2.0 -->

# Why Brunnr (and how it relates to TencentDB Agent Memory)

Brunnr's memory layer deliberately borrows proven ideas from
[TencentDB Agent Memory](https://github.com/TencentCloud/TencentDB-Agent-Memory) — L0–L3
progressive tiering, hybrid BM25 + vector retrieval fused with RRF, markdown white-box records,
and `node_id` drill-down for context offloading. That project is excellent and we credit it. So
why does Brunnr exist?

**Brunnr is not a memory provider — it is a multi-agent context orchestration system.** Memory is
one module. The honest differences, and where Brunnr aims to be strong:

| | TencentDB Agent Memory | Brunnr |
|---|---|---|
| Scope | Memory only (capture → extract → recall) | Memory **+ task tracking + master/worker/judge orchestration + sandbox** |
| Integration | OpenClaw plugin / Hermes provider (framework-coupled) | **MCP-first, agent-agnostic** (Claude Code, Codex, Zed, opencode, …) + pluggable `Agent` adapters |
| Runtime | Node ≥22.16 + TypeScript | **Rust** — single static binary, no runtime |
| Vector store | SQLite + sqlite-vec (local-first; remote on roadmap) | **Pluggable `VectorStore`**: Files(OKF) / sqlite-vec / Qdrant (+ TencentDB-style adapter possible) |
| On-disk format | bespoke markdown/JSONL layout | **Open Knowledge Format (OKF)** — vendor-neutral, portable, interop with the OKF ecosystem |
| Concurrency | single-user, local-first | **multi-project + multi-user + parallel** (collection-per-project + payload tenancy) — see [concurrency.md](concurrency.md) |
| Cross-tool memory | within its host framework | **neutral shared store both Claude Code and Codex read** (their native memories are siloed) |
| Upgrades | — | **upgrade-survivable**: OKF = source of truth, Qdrant = rebuildable index, `migrate` + version metadata ([upgrades.md](upgrades.md)) |
| Orchestration safety | n/a (not an orchestrator) | **verifiers-as-trust-boundary, judge-sole-committer, task DAG, worker workspace isolation** |

What Brunnr reuses from TencentDB (with credit): the L0–L3 tiering, hybrid+RRF retrieval, the
markdown white-box principle, `node_id` drill-down, and the benchmark-rigor mindset. A
TencentDB-style symbolic Mermaid "task canvas" for short-term memory is a natural future addition
on top of Brunnr's WorkingMemory + session anchor.

**One-line positioning:** if you want *just memory*, several good options exist (including
TencentDB). Brunnr is for teams/operators who want a **non-intrusive, Rust, MCP-first system that
spans the whole agent loop** — memory you can reuse on its own *and* optional orchestration that
shares the same store — across multiple agents, projects, and users, surviving its own upgrades.
Use as little (just `memory` mode) or as much (`full`) as you want.
