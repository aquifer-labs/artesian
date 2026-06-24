<!-- SPDX-License-Identifier: Apache-2.0 -->
---
title: Artesian — Memory Control Plane
description: An open, portable, governed memory control plane for AI agents. OCF is the open format.
hide:
  - navigation
  - toc
---

<div class="art-hero">
  <img src="brand/artesian-mark.svg" class="art-logo" alt="Artesian">
  <p class="art-tagline">
    An open, portable, governed memory control plane for AI agents.
  </p>
  <p class="art-sub">OCF is the open format. Nothing is lost.</p>
  <div class="art-actions">
    <a href="onboarding/" class="md-button md-button--primary">Get started</a>
    <a href="https://github.com/aquifer-labs/artesian" class="md-button">GitHub</a>
  </div>
</div>

## Install

=== "Homebrew"

    ```shell
    brew install aquifer-labs/tap/artesian
    ```

=== "From source"

    ```shell
    cargo install --git https://github.com/aquifer-labs/artesian artesian-cli
    artesian init --backend sqlite-vec   # zero infrastructure
    ```

=== "MCP drop-in"

    Add to `claude_desktop_config.json` or your MCP settings:

    ```jsonc
    {
      "artesian-memory": {
        "command": "artesian-mcp",
        "args": ["--config", "artesian.toml"]
      }
    }
    ```

    `artesian init` writes the config. Works with Claude Code, Codex, opencode, and any MCP client.

---

<div class="art-token-banner">
  <div class="art-token-stat">
    <span class="art-token-before">10,144</span>
    <span class="art-token-arrow">→</span>
    <span class="art-token-after">1,260</span>
    <span class="art-token-unit">tokens</span>
  </div>
  <div class="art-token-desc">
    <strong>Token savings — measured on every recall.</strong>
    Artesian records baseline vs returned tokens on every <code>loop.recall</code> and
    <code>memory.context</code> call. The example above is a single session; your live
    total updates in real time.
    Run <code>artesian tokens</code> to see your own numbers, or query
    <code>memory.savings</code> from MCP.
    <a href="token-savings/">Full details →</a>
  </div>
</div>

---

## Three differentiators

<div class="grid cards" markdown>

-   :material-check-decagram:{ .lg .middle } **OCF governed committed-state**

    ---

    Every memory write passes through a **qualify-gate** — drift, novelty, and relevance checks
    before it enters the Committed Context State (CCS). Evictions and compactions are recorded
    in the OCF qualify trail; forgetting is as auditable as remembering.

    [:octicons-arrow-right-24: Governance & qualify trail](governance.md)

-   :material-transit-connection-variant:{ .lg .middle } **Multi-writer + cross-agent handoff**

    ---

    Transactional multi-writer with per-scope isolation and optimistic CAS — no silent
    corruption under concurrent writes. A portable working-context kit lets any agent
    (Claude Code, Codex, Gemini, opencode) pick up exactly where another left off.

    [:octicons-arrow-right-24: Concurrency model](concurrency.md)

-   :material-language-rust:{ .lg .middle } **Tiny binary · zero cloud · local embeddings**

    ---

    A single static Rust binary (~29 MB, stripped). E5-small embeddings run entirely
    in-process — no GPU, no network, no token cost to embed. Backends: sqlite-vec
    (zero infra), Qdrant, pgvector, or plain files.

    [:octicons-arrow-right-24: Why Rust](why-rust.md)

</div>

---

## How it works

```
recall candidates (from durable memory)
        │
        ▼  qualify-gate: drift / novelty / relevance
┌───────────────────────────────────────┐
│   Committed Context State (bounded)   │  ← what the agent sees
│   ┌───────────────────────────────┐   │
│   │ decision:  chose Rust+tokio   │   │
│   │ plan:      shard the embedder │   │
│   │ blocker:   GPU quota at limit │   │
│   └───────────────────────────────┘   │
└───────────────────────────────────────┘
        │ evict / compress under saturation
        ▼
durable memory (sqlite-vec / Qdrant / pgvector / files)
        ↑ anchor + targeted recall on any compaction
```

Artesian implements the **ACC model** (Bousetouane, arXiv:2601.11653): a control loop that
separates the *recall* channel (read from any retrieval store) from the *commit* channel
(what enters the bounded, schema-governed CCS). The qualify-gate sits between them.

[:octicons-arrow-right-24: Full architecture](architecture.md) ·
[:octicons-arrow-right-24: Why Artesian?](positioning.md)

---

## Governance — nothing is lost

<div class="art-highlight-grid">
<div class="art-highlight-box">
<h3>:material-shield-check: Qualify trail</h3>
<p>
Every commit, eviction, drift decision, and dream-on-compact run writes a signed entry
to the OCF qualify trail (<code>qualify.jsonl</code>). The agent's memory history is a
complete audit log — you can replay or verify any past state.
</p>
<a href="governance/">Read the governance page →</a>
</div>
<div class="art-highlight-box">
<h3>:material-open-source-initiative: Open format: OCF</h3>
<p>
The Open Committed-state Format (OCF) is the portable, schema-governed exchange format
underlying Artesian's kit export, session handoff, and qualify trail. It is defined as
an open spec so any tool can read, write, and verify Artesian memory.
</p>
<a href="https://github.com/aquifer-labs/ocf">OCF spec on GitHub →</a>
</div>
</div>

---

## Comparison

| | **Artesian** | mem0 | LangMem | plain markdown |
|---|---|---|---|---|
| **What it is** | ACC control plane + memory | Managed memory API | LangGraph memory layer | Files + prompting |
| **Self-hosted, zero infra** | ✓ sqlite-vec or files | Cloud-first | Requires LangGraph | ✓ |
| **No per-write LLM call** | ✓ | ✗ | ✗ | ✓ |
| **ACC qualify-gate** | ✓ drift/novelty/relevance | ✗ | ✗ | ✗ |
| **Compaction survival** | ✓ anchor + recall | ✗ | ✗ | ✗ |
| **Multi-writer, isolated** | ✓ optimistic CAS | partial | partial | ✗ |
| **Agentic eval harness** | ✓ ships in repo | ✗ | ✗ | ✗ |
| **Open format (OCF)** | ✓ | ✗ | ✗ | ✗ |

---

## Composable by design

Artesian is built like LEGO — use only the pieces you need:

| Layer | Crate | When to use |
|---|---|---|
| Memory + qualify-gate | `aquifer` + `headgate` | You want ACC control over any vector store |
| Task tracking | `headrace` | DAG-based task board, no orchestration needed |
| Agent teams | `flume` | Multi-agent with shared memory and handoff |
| Eval harness | `gauge` | Measure recall quality and memory-guides-action |
| MCP server | `artesian-mcp` | Drop-in for Claude Code, Codex, opencode |
| Full stack | `artesian-cli` | `artesian init`, `loop`, `kit`, `tokens`, `task` |

[:octicons-arrow-right-24: Full composability guide](composability.md)

---

## Benchmarks

| Memory / history | Full-context replay | Artesian | Saving | Retrieved |
|---|---:|---:|---:|---:|
| ~13k tokens (180 docs) | 12,902 | 876 tokens | 93% | 100% |
| ~119k tokens (1,600 docs) | 118,566 | 974 tokens | 99.2% | 100% |
| ~478k tokens (6,400 docs) | 477,740 | 992 tokens | 99.8% | 100% |
| ~1M tokens (14,000 docs) | 1,046,431 | 1,046 tokens | **99.9%** | 100% |

Reproduce: `just bench-check` — fails if committed results differ.

| Benchmark | Score | Method |
|---|---:|---|
| LoCoMo | 0.475 | vector + BGE reranking |
| LongMemEval (oracle) | 0.70 | vector retrieval |

[:octicons-arrow-right-24: Full benchmark methodology](positioning.md#how-it-compares)

---

*Apache-2.0 · [Aquifer Labs](https://github.com/aquifer-labs) · [OCF spec](https://github.com/aquifer-labs/ocf)*
