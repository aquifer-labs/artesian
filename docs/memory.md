<!-- SPDX-License-Identifier: Apache-2.0 -->

# Mímisbrunnr — Brunnr Memory Architecture

> *Mímisbrunnr is the well of wisdom at the root of Yggdrasil. Brunnr's memory subsystem is the
> well your agents drink from before they act.*

This document describes how Brunnr models, stores, and retrieves agent memory: the taxonomy
(short-term vs long-term), the concrete data model, the retrieval mathematics as actually
implemented, the pluggable backend seam, and the self-repair mechanism that survives context
auto-compaction. Formulas here match the code; each section links to the implementing file.

Status legend: **[implemented]** = present in `crates/mimisbrunnr`; **[planned]** = designed,
not yet built (tracked in the roadmap).

---

## 1. Why memory

An LLM call is stateless: the model only sees its context window of size $L_{context}$. Two
pressures follow for any agent that runs longer than one prompt:

1. **Recall beyond $L_{context}$** — facts, decisions, and past work exceed the window.
2. **Token economy** — replaying whole project docs into every session is expensive and
   crowds out reasoning budget.

Brunnr's thesis: replace *replaying everything* with *retrieving only what is relevant*. A
targeted recall of a few hundred tokens substitutes for thousands of tokens of re-read context,
which is both cheaper and more focused. This is the same lever the Brunnr project itself was
created to pull.

> **Design principle — cheap by default, smart on demand.** The default memory path is a local
> embedding plus a rank fusion: no extra LLM calls, ~tens of ms, zero added tokens. Every
> sophistication that costs an LLM call or real latency (reranking, HyDE, multi-query,
> reflection, consolidation) is **opt-in** and **off by default**, so "memory mode" never changes
> how you drive your agent — it only makes it cheaper and sharper.

```mermaid
flowchart LR
  subgraph STM["Short-term memory [planned]"]
    W[Working buffer: recent turns]
    A[Session anchor: task, plan ptr, next step]
  end
  subgraph LTM["Long-term memory [implemented]"]
    V[(Vector store: tiered records)]
    F[(Files: date-tagged md)]
  end
  Agent[Agent core] -->|store durable learnings| LTM
  Agent -->|recent state| STM
  STM -->|consolidate / summarize| LTM
  LTM -->|find: top-k relevant slice| Agent
  A -->|self-repair after compaction| Agent
```

---

## 2. Taxonomy and data model

Brunnr follows the standard agent-memory split (short-term working memory vs long-term
persistent memory; see references) and layers a TencentDB-style tiering over long-term records.

Every long-term unit is a `MemoryRecord` (`crates/mimisbrunnr/src/types.rs`):

| Field | Meaning |
|---|---|
| `id` | content-derived stable id (dedup key) |
| `node_id` | deterministic handle for drill-down from a summary to ground truth |
| `content` | the embedded text |
| `tags`, `metadata` | filterable payload |
| `tier` | `L0Raw` \| `L1Atom` \| `L2Scenario` \| `L3Project` |
| `created_at` | timestamp (date-tagged on disk) |

**Memory tiers** (`MemoryTier`) implement progressive abstraction so high-level summaries stay
traceable to evidence:

```mermaid
flowchart TD
  L0["L0Raw — raw logs / transcripts"] --> L1["L1Atom — atomic facts"]
  L1 --> L2["L2Scenario — aggregated scenarios"]
  L2 --> L3["L3Project — distilled project/persona knowledge"]
  L3 -. node_id drill-down .-> L0
```

A consumer asks at a high tier (cheap, dense) and uses `get_node(node_id)` to pull the exact
ground-truth record only when needed — "context offloading" that keeps prompts small.

---

## 3. Long-term memory — semantics and math [implemented]

### 3.1 Embeddings

Text is embedded into a dense vector $e = f(\text{text})$ with a pinned model so vectors are
comparable across every machine and backend
(`crates/mimisbrunnr/src/vector_memory.rs`):

- model `intfloat/multilingual-e5-small`, $d = 384$ dimensions, multilingual (RU/EN);
- E5 requires asymmetric prefixes — Brunnr embeds queries as `query: …` and stored passages as
  `passage: …` (`FastembedTextEmbedder::embed_query` / `embed_passage`). Skipping the prefixes
  measurably degrades E5 recall, so this is enforced in one place.

### 3.2 Semantic similarity

Relevance between a query embedding $q$ and a record embedding $d$ is cosine similarity (the
configured `Distance::Cosine`):

```math
\operatorname{sim}(q, d) = \cos\theta = \frac{q \cdot d}{\lVert q\rVert\,\lVert d\rVert}
= \frac{\sum_{i=1}^{384} q_i d_i}{\sqrt{\sum_{i=1}^{384} q_i^2}\,\sqrt{\sum_{i=1}^{384} d_i^2}}
```

Range $[-1, 1]$; higher means more semantically related. The vector store performs **approximate
nearest-neighbour (ANN)** search to return the top-$k$ records by this metric without scanning
every point — single-digit milliseconds at Brunnr's scale.

### 3.3 Hybrid retrieval via Reciprocal Rank Fusion

Pure vector search misses exact tokens (identifiers, error codes); pure keyword search misses
paraphrase. Brunnr runs **both channels** and fuses them with Reciprocal Rank Fusion
(`crates/mimisbrunnr/src/rrf.rs`). For a document $d$ appearing at rank $r_c(d)$ (1-based) in
channel $c$:

```math
\operatorname{RRF}(d) = \sum_{c \in \{\text{keyword},\,\text{vector}\}} \frac{1}{k + r_c(d)},
\qquad k = 60
```

The code computes `1.0 / (rank_constant + rank + 1.0)` with 0-based `rank` and
`rank_constant = 60` (`RrfOptions::default`, `types.rs`), i.e. exactly $1/(k + r_c)$ with 1-based
$r_c$. Documents are summed across channels, sorted by fused score (ties broken by `node_id` for
determinism), and truncated to the limit.

Why RRF and $k=60$: RRF needs only *ranks*, not calibrated cross-channel scores, so a BM25 score
and a cosine score combine without normalization; $k=60$ is the original constant from Cormack et
al. (2009) and damps the influence of low ranks. This is why one fusion function works unchanged
across every backend.

### 3.4 Retrieval and store flows

```mermaid
sequenceDiagram
  participant Ag as Agent
  participant VM as VectorMemoryBackend
  participant Em as Fastembed (e5-small)
  participant VS as VectorStore (qdrant | sqlite-vec)
  Ag->>VM: find(query)
  VM->>VS: keyword search (BM25/payload)
  VM->>Em: embed_query("query: …")
  VM->>VS: vector ANN search (cosine)
  VM->>VM: reciprocal_rank_fusion([kw, vec], k=60)
  VM-->>Ag: top-k SearchHit (record, score, source=hybrid)
  Ag->>VM: store(memory)
  VM->>VM: stable_memory_id (content hash) — dedup
  VM->>Em: embed_passage("passage: …")
  VM->>VS: upsert(point{id, vector, payload})
```

`store` is idempotent: `stable_memory_id` hashes content, and an existing id short-circuits the
upsert (`vector_memory.rs`), so re-running a backfill never duplicates. This gives the
idempotency the literature calls for in heterogeneous memory writes.

### 3.5 Optional retrieval enhancements [planned, opt-in]

The default path (embed + RRF) is intentionally cheap. For precision-critical or ambiguous
queries, Brunnr will expose higher-quality stages behind config flags — each adds latency and
sometimes an LLM call, so each is off by default:

- **Two-stage reranking** — retrieve a wider candidate set of $M \gg k$ by RRF, then re-score
  with a cross-encoder (`Reranker` seam; a local fastembed reranker, no API), keep top-$k$.
  Cross-encoders read query+document jointly and are more accurate than bi-encoder cosine alone.
- **HyDE** — embed a hypothetical answer $d_{hypo} = \text{LLM}(q)$ instead of the bare query, to
  close the query-document vocabulary gap (one upfront LLM call).
- **Multi-query** — expand $q$ into $\{q_1..q_N\}$, search each, merge+dedup for topic coverage.

These map onto the same `VectorStore`/RRF stack; only the query/scoring stage changes.

---

## 4. Pluggable backends [implemented]

Memory is engine-agnostic. The top contract is `MemoryBackend`
(`crates/mimisbrunnr/src/backend.rs`); the vector engines sit behind the thin `VectorStore` seam
(`crates/mimisbrunnr/src/vector.rs`) and the generic `VectorMemoryBackend<V>` writes embedding,
tiering, RRF, and payload schema **once** for all of them.

```mermaid
flowchart TD
  MB[trait MemoryBackend] --> FB[FilesBackend — md, date-tagged]
  MB --> VMB["VectorMemoryBackend&lt;V: VectorStore&gt;"]
  VMB --> QS[QdrantVectorStore]
  VMB --> SS[SqliteVecVectorStore]
  VMB -. future .-> TS[TencentDBVectorStore]
```

| Backend | Keyword channel | Vector channel | Hybrid | Infra |
|---|---|---|---|---|
| `FilesBackend` | substring over md | — | n/a (keyword only) | none |
| `SqliteVecVectorStore` | FTS5 **BM25** | `vec0` cosine | **RRF** (client) | none (embedded) |
| `QdrantVectorStore` | payload match¹ | HNSW cosine | **RRF** (client) | Qdrant server |

¹ Both vector backends currently report `supports_server_side_hybrid = false`
(`sqlite_vec.rs`, `qdrant.rs`), so fusion runs client-side via RRF. `capabilities()` is the seam
for future server-side fusion (Qdrant Query-API / sparse vectors); when a backend sets the flag,
`VectorMemoryBackend::hybrid_rrf` delegates to the engine instead. sqlite-vec already gives a true
BM25 keyword channel; Qdrant's keyword channel stays payload-match until sparse/BM25 is added
**[planned]**.

The normalized `Filter` (`eq`/`in`/`range`/`exists` with `must`/`should`/`must_not`,
`types`/`vector.rs`) is the only query DSL consumers touch; each adapter translates it to native
filters. Engine specifics (metric names, id types, index knobs) never leak above `VectorStore`.

---

## 5. Short-term memory [planned]

Long-term memory answers "what do we know"; short-term memory answers "what are we doing right
now". Brunnr will provide three standard mechanisms (see references) behind a `WorkingMemory`
seam:

- **buffer** — full recent turns (highest fidelity, smallest horizon);
- **sliding window** — last $k$ turns, bounded prompt contribution;
- **summary buffer** — periodically summarize older turns with the judge/worker model, keep
  recent turns verbatim. Summaries are written down as `L2Scenario`/`L3Project` records, so
  *short-term consolidation feeds long-term memory* rather than being discarded.

```mermaid
flowchart LR
  T[New turn] --> WB[Working buffer]
  WB -->|len > k| SW[Sliding window: keep last k]
  SW -->|older turns| SUM[Summarize]
  SUM -->|write as L2/L3 record| LTM[(Long-term memory)]
```

This consolidation path is what makes recall *improve* over time instead of growing unbounded.

---

## 6. Self-repair across auto-compaction [partly planned]

Long sessions hit auto-compaction: the host summarizes/truncates context and the agent can lose
its place. Brunnr makes this a non-event (see `docs/self-repair.md`):

1. **Session anchor (Muninn)** — a tiny, always-current record of the in-flight task, the plan
   pointer, the last N decisions, and the next concrete step. Cheap to write every turn.
2. **Continuous externalization** — durable learnings are flushed to long-term memory as they
   occur, so truncation loses nothing recoverable via `find`.
3. **Self-repair hook** — on a detected compaction/resume boundary the agent re-reads the anchor
   (deterministic) and runs a targeted `find` (semantic) before its next action — no manual
   "re-read the docs" step.

```mermaid
flowchart LR
  C{Compaction / resume?} -->|yes| RA[Read session anchor]
  RA --> RC["find(current task) — top-k recall"]
  RC --> ACT[Resume exact next step]
  C -->|no| ACT
```

The deterministic anchor handles "what is my current step" (vector search is too fuzzy for that);
the semantic recall restores the surrounding knowledge. Together they make a switch between
agents (e.g. Claude Code → Codex) lossless.

---

## 7. Token-efficiency rationale

Let a project's full re-read context cost $T_\text{full}$ tokens. Replaying it every session of a
multi-session task of $N$ sessions costs $\approx N \cdot T_\text{full}$. Retrieval instead
injects only the top-$k$ slices, $\approx k \cdot \bar{c}$ tokens ($\bar{c}$ = mean chunk size),
plus the anchor $T_a \ll T_\text{full}$:

```math
\text{savings per session} \approx T_\text{full} - (k\,\bar{c} + T_a)
```

Tiering compounds this: high-tier (`L3Project`) records are dense summaries, and `get_node`
drill-down fetches raw evidence only on demand, so the common path pays for summaries, not
transcripts. Local embedding (fastembed) adds bounded latency (~10–50 ms/query) and **zero
tokens**, so the retrieval path is a net token win, not a cost.

---

## 8. Memory lifecycle — consolidation, decay, pruning [planned, opt-in]

Unbounded growth raises latency and dilutes relevance, so a long-lived memory needs active
management. Brunnr will run these as an **optional, asynchronous, off-by-default** consolidation
pass (never on the agent's critical path; not active in plain memory mode unless enabled):

- **Reflection / consolidation** — periodically summarize recent `L0Raw`/`L1Atom` records into
  higher-tier `L2Scenario`/`L3Project` records (Generative-Agents reflection). This is exactly
  the short-term → long-term path in §5 and what makes recall *improve* over time.
- **Recency/importance decay** — each record carries a score that decays with age,
  ```math
  S_{new} = S_{old}\cdot e^{-\lambda \Delta t}
  ```
  used to bias ranking and to select prune candidates (low score + rarely retrieved).
- **Redundancy elimination** — beyond exact content-hash dedup, merge semantically near-duplicate
  records (high cosine), keeping the highest-tier representative + `node_id` links.
- **Triggering** — time-, event-, or resource-based; runs offline/async to avoid blocking the
  agent. Fidelity vs. compactness is a tunable, per the references.

Default stance stays non-intrusive: with consolidation off, memory is an append-and-retrieve
store; turning it on adds curation without changing how the agent is driven.

## References

- Reimers & Gurevych, *Sentence-BERT* (EMNLP-IJCNLP 2019) — semantically meaningful sentence
  embeddings. https://aclanthology.org/D19-1410/
- Wang et al., *Multilingual E5 Text Embeddings* (2024) — the `query:`/`passage:` prefix
  convention used here. https://arxiv.org/abs/2402.05672
- Malkov & Yashunin, *HNSW* (SDM 2018) — the ANN index class vector stores use.
  https://arxiv.org/abs/1603.09320
- Johnson et al., *Billion-scale similarity search (FAISS)* (CVPR 2017).
  https://ieeexplore.ieee.org/document/8099859
- Cormack, Clarke & Büttcher, *Reciprocal Rank Fusion outperforms Condorcet and individual rank
  learning methods* (SIGIR 2009) — RRF and the $k=60$ constant.
  https://plg.uwaterloo.ca/~gvcormack/cormacksigir09-rrf.pdf
- Park et al., *Generative Agents* (2023) — memory stream + reflection (short-term/long-term
  integration; decay-based importance). https://arxiv.org/abs/2304.03442
- Nogueira & Cho, *Passage Re-ranking with BERT* (2019) — cross-encoder reranking stage.
  https://arxiv.org/abs/1901.04085
- Gao et al., *Precise Zero-shot Dense Retrieval without Relevance Labels (HyDE)* (EMNLP 2022).
  https://arxiv.org/abs/2212.10496
- Lewis et al., *Retrieval-Augmented Generation* (NeurIPS 2020) — the RAG read interface.
  https://arxiv.org/abs/2005.11401
- Yao et al., *ReAct* (ICLR 2023) — explicit retrieve/act triggers.
  https://arxiv.org/abs/2210.03629
- LangChain Memory & LlamaIndex Memory — practical short-term buffer/window/summary mechanisms.
- TencentDB Agent Memory — L0–L3 tiering, hybrid recall, node_id drill-down.
  https://github.com/TencentCloud/TencentDB-Agent-Memory
- ApX, *Agentic LLM Systems & Memory Architectures*, Chapter 3 — conceptual framing.
  https://apxml.com/courses/agentic-llm-memory-architectures
