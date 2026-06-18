<!-- SPDX-License-Identifier: Apache-2.0 -->

# ACC control-quality benchmark

Retrieval benchmarks (see [../benchmarks/README.md](../benchmarks/README.md)) measure *finding*
the right records. This benchmark measures what a **memory controller** must additionally get
right: keeping the committed context **small**, **on-topic**, and **free of drift and
hallucination**. It lives in the `gauge` crate (`crates/gauge/src/bench.rs`) and runs over a
labeled set of recall candidates.

## Metrics

Each candidate fact carries a ground-truth `FactLabel`:

| label | meaning | admitting it is… |
|---|---|---|
| `gold` | relevant and true | correct |
| `distractor` | irrelevant noise | wasted footprint |
| `contradiction` | contradicts a gold fact | **drift** |
| `fabrication` | unsupported / invented | **hallucination** |

One ACC cycle is run and the committed state is scored:

- **footprint_tokens** — tokens in the committed context (cl100k_base).
- **footprint_ratio** — `footprint_tokens / raw_recall_tokens`; the share of the raw recall
  dump that actually gets committed (lower is better). This is the token-efficiency moat number,
  directly comparable to a system's reported tokens/query.
- **precision / recall** — over the gold facts.
- **drift_rate** — fraction of admitted facts labeled `contradiction`.
- **hallucination_rate** — fraction of admitted facts labeled `fabrication`.

Footprint and the label-based rates are **deterministic**, so the default-gate arm runs in CI.

## Arms

- **default-gate** — the deterministic gate (relevance threshold + redundancy rejection). It
  earns the footprint saving and catches near-duplicate contradictions, but cannot detect a
  plausible fabrication.
- **judge-gate** (feature `llm`) — the LLM judge-eval gate scoring relevance / novelty / drift.
  It is what drives `drift_rate` and `hallucination_rate` toward zero.

Run the deterministic report:

```shell
cargo run -p gauge --bin gauge-bench            # markdown
cargo run -p gauge --bin gauge-bench -- --json  # JSON
```

## Competitor-comparable evaluation (LoCoMo / LongMemEval, vs mem0)

The metric shapes line up with the agent-memory literature so results are comparable:

- `footprint_tokens` ↔ mem0's reported **tokens/query** (mem0 ≈ 6.7 k/query; Artesian commits a
  bounded budget, default 2048, and reports the actual committed footprint).
- `precision` / `recall` ↔ LoCoMo / LongMemEval answer scoring.
- `drift_rate` / `hallucination_rate` have no analogue in retrieval-only benchmarks — they are
  the memory-**control** axis.

To evaluate a real dataset, load each conversation turn's candidate facts and their
gold/contradiction labels into a `BenchCase` (the shape `demo_case()` returns), run both arms,
and aggregate `BenchResult` across cases. A full head-to-head with mem0 additionally runs mem0's
own pipeline on the same corpus and compares tokens/query and precision; that harness is external
to this repo (it needs the mem0 runtime) and is tracked separately.
