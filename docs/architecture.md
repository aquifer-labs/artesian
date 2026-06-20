<!-- SPDX-License-Identifier: Apache-2.0 -->

# Architecture Overview

Artesian is a multi-agent context orchestration system: pluggable **memory**, optional
**orchestration** (master/worker/judge), optional **task tracking**, and optional **sandboxing** —
all **non-intrusive**. It integrates with agents over **MCP**, so any MCP-capable tool (Claude
Code, Codex, Zed, opencode, …) gains Artesian's capabilities without changing how it is driven. You
adopt only what you want via [modes](modes.md).

This page is the map; each concern has its own doc.

## System map

[![System map](diagrams/system-map.png)](diagrams/system-map.mmd)

## Crates (strict boundaries, trait seams)

| Crate | Responsibility |
|---|---|
| `artesian-core` | roles (master/worker/judge), task-queue types (Job/Queue/CompletedJob), config, modes, the `Agent` adapter trait, the event envelope |
| `aquifer` | memory: `MemoryBackend`, the `VectorStore` seam, `VectorMemoryBackend<V>`, RRF, tiers, OKF files |
| `headgate` | ACC control plane: `RecallStore` data-plane seam, `QualifyGate`, bounded `CommittedContextState`, commit-loop controller, `GaugeMetrics`; feature `llm` adds the LLM judge-eval gate, `CouncilJudge` (panel + arbiter), LLM compressors, and local provider aliases (Ollama/LM Studio/mlx); feature `headroom` adds `HeadroomCompressor` adapter |
| `headrace` | task tracking: `TaskStore` (Files/Vector/External), the task DAG |
| `artesian-mcp` | exposes tools over MCP (`memory.*`, `tools.find`, task tools); the agent integration point |
| `artesian-cli` / `artesiand` | user entrypoint + optional daemon (init, memory ops, spawn, pooling) |
| `gauge` | evaluation harness: LoCoMo/LongMemEval recall benchmarks, agentic task scoring (memory-guides-action), footprint/accuracy/token metrics, LLM-as-judge eval (see [acc-bench.md](acc-bench.md)) · `sandbox` optional Docker sandbox |

Engine/agent/tracker specifics live behind traits (`VectorStore`, `Agent`, `TaskStore`,
`MemoryBackend`, `RecallStore`, `QualifyGate`, `Compressor`) so adding a backend, agent, tracker,
retrieval store, gate, or compressor is a small adapter, never a core change.

## Cross-cutting concerns (read the focused docs)

- **Memory** — short/long-term, retrieval math (cosine, RRF k=60), tiers L0–L3, OKF on-disk
  format, optional rerank/HyDE/consolidation. → [memory.md](memory.md)
- **Concurrency & multi-tenancy** — many agents/users in parallel; append-mostly idempotent
  writes, project-per-collection + payload tenancy, backend-by-concurrency. → [concurrency.md](concurrency.md)
- **Orchestration & coordination** — roles, topologies, router (agent + semantic tool selection),
  event envelope, coordination mechanisms, worker workspace isolation, verifiers, observability.
  → [orchestration.md](orchestration.md)
- **Task tracking** — DAG with dependencies, hierarchical decomposition, md/vector/external
  (Jira/Linear). → [task-tracking.md](task-tracking.md)
- **Self-repair** — surviving auto-compaction via a deterministic anchor + recall. → [self-repair.md](self-repair.md)
- **Modes** — `memory` | `orchestrate` | `full` | `advanced` (BYO). → [modes.md](modes.md)
- **Context tree** — layered, priority-ordered AGENTS/CLAUDE md. → [context-tree.md](context-tree.md)
- **Build & contribute** — [development.md](development.md)

## Design invariants

1. **Non-intrusive default.** `memory` mode adds only `memory.find`/`memory.store` over MCP; the
   agent workflow is unchanged. Anything costing an LLM call or latency is opt-in, off by default.
2. **MCP-first.** One universal integration surface for every agent tool.
3. **Trait seams, thin adapters.** Backends/agents/trackers are pluggable; the core is engine-
   agnostic.
4. **Append-mostly, idempotent memory.** No read-modify-write on points ⇒ safe concurrency.
5. **Single mutation authority + blackboard.** Agents coordinate indirectly through shared
   memory + the task DAG, not token-heavy chatter; one authority serializes state changes.
6. **Verifiers define trust.** The judge commits only when configured verifiers pass.
