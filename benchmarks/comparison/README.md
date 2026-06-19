<!-- SPDX-License-Identifier: Apache-2.0 -->

# Competitor-comparable QA benchmark (LoCoMo / LongMemEval, vs mem0)

This harness produces the **Artesian** side of a head-to-head on the two public agent-memory
QA datasets the literature reports — **LoCoMo** and **LongMemEval** — in the same shape mem0
publishes (answer accuracy via LLM-as-judge, and **tokens/query**). It is intentionally *not* a
re-run of mem0: mem0 is a Python system that needs cloud LLM calls and its own infra, so its
numbers are cited from the published paper and compared against, rather than reproduced here.

## What it measures

For each question the runner stores the conversation as recall candidates, runs one **ACC
cycle** to build a bounded committed context, asks the LLM to answer **only from that context**,
then grades the answer against gold with the same LLM (LLM-as-judge — the standard protocol):

- **accuracy** — graded-correct fraction (comparable to LoCoMo / LongMemEval "J" scores).
- **mean tokens/query** — committed-context tokens fed to the answerer; the token-efficiency
  number directly comparable to mem0's reported per-query token budget.
- **footprint_ratio** — committed tokens ÷ raw recall dump.

Retrieval defaults to lexical (term-overlap) recall over the case facts: deterministic and
dependency-free. Swap in a vector backend for a production-grade retrieval run.

## Honesty notes

- mem0 numbers must be quoted from its **paper** (Chhikara et al., *"Mem0: Building
  Production-Ready AI Agents with Scalable Long-Term Memory"*, arXiv:2504.19413, 2025) and read
  off its tables — do **not** trust second-hand figures (including any earlier draft numbers in
  `docs/positioning.md`, which should be re-verified against the source before publication).
- Cross-system comparison is only fair under a matched protocol: same dataset split, same judge
  model, same retrieval budget. State the judge model and budget with any published result.
- The Artesian numbers below were produced by this harness; the mem0 column is left to be filled
  from the cited paper under the same judge/budget you run Artesian with.

## Datasets (download separately)

Not vendored here (size + licensing). Fetch into `benchmarks/comparison/data/`:

- **LoCoMo** — <https://github.com/snap-research/locomo> (`locomo10.json`).
- **LongMemEval** — <https://github.com/xiaowu0162/LongMemEval> (`longmemeval_s.json` /
  `longmemeval_oracle.json`; also on Hugging Face).

The loaders are tolerant of the public schemas (numbered `session_N` turns for LoCoMo;
`haystack_sessions` for LongMemEval) and report how many malformed entries were skipped.

## Running

The answering/grading LLM is reached through a command. The default wraps `codex exec`
(`benchmarks/comparison/codex-complete`, model `gpt-5.5`, reasoning `xhigh`; override with
`CODEX_MODEL` / `CODEX_REASONING`). Any OpenAI-compatible endpoint works too — point
`--llm-command` at your own wrapper.

```shell
# vector recall (real embedding retrieval) needs the `vector` feature; lexical needs only `llm`.
cargo build -p gauge --features "llm vector" --bin gauge-eval

# --recall lexical (default, deterministic) | vector (embedding + RRF). Scale up with --limit.
./target/debug/gauge-eval locomo      benchmarks/comparison/data/locomo10.json          --limit 50 --recall vector
./target/debug/gauge-eval longmemeval benchmarks/comparison/data/longmemeval_oracle.json --limit 50 --recall vector --json
```

## Results

Judge = `codex` gpt-5.5 (reasoning `xhigh`), Artesian **vector recall** (real
`VectorMemoryBackend`: embedding + small-to-big + RRF), LongMemEval on the **oracle** split.
(`graded` < n where a `codex` call errored and that case was skipped.)

### Large sample (the robust numbers)

| dataset | n | accuracy | tokens/query | footprint vs full |
|---|---|---|---|---|
| LoCoMo | 200 | **0.370** (74/200) | 534 | 0.039 |
| LongMemEval (oracle) | 500 (full split) | **0.699** (348/498) | 1944 | 0.343 |

**mem0** (arXiv:2504.19413, cited — not re-run here; no OpenAI API key available, and mem0's
pipeline is not faithfully reproducible through the `codex` ChatGPT-OAuth CLI). The paper reports
*relative* figures only: **+26 %** LLM-as-judge over OpenAI memory on LoCoMo, **91 %** lower p95
latency, **> 90 %** token savings vs. a full-context baseline. A same-protocol head-to-head needs
mem0 run with a real OpenAI key under this judge/budget.

### Recall ablation (n = 30, shows the lexical→vector lift)

| dataset | recall | accuracy | tokens/query | footprint |
|---|---|---|---|---|
| LoCoMo | lexical | 0.103 (3/29) | 671 | 0.047 |
| LoCoMo | **vector** | **0.276 (8/29)** | 524 | 0.037 |
| LongMemEval (oracle) | lexical | 0.621 (18/29) | 2064 | 0.288 |
| LongMemEval (oracle) | **vector** | **0.867 (26/30)** | 2052 | 0.286 |

**Reading these honestly:**

- **Vector recall is the accuracy win.** Switching from lexical (term-overlap) to embedding +
  small-to-big + RRF roughly triples LoCoMo and clearly lifts LongMemEval — the lexical default
  is a floor, as expected, because it misses paraphrased evidence.
- **Trust the large sample, not n = 30.** The 30-question slice was optimistic (LongMemEval 0.87
  → **0.70** on the full split; LoCoMo 0.28 → **0.37** at n = 200). The de-noised figures are
  LoCoMo ≈ 0.37 and LongMemEval-oracle ≈ 0.70. LoCoMo stays harder (long multi-session temporal
  reasoning, exact-value answers).
- **Token efficiency holds throughout.** Committed context is **~3.9 %** of the full LoCoMo
  conversation and **~34 %** of the LongMemEval-oracle history — the bounded-footprint property
  a memory *controller* is for, and the axis comparable to mem0's "vs full-context" savings.
  Vector recall even *reduces* committed tokens (524 vs 671 on LoCoMo) by surfacing fewer, more
  relevant facts.
- **Caveats:** n = 30 (still a sample, some noise); strict LLM-as-judge; LoCoMo answers are often
  exact dates/values; oracle split for LongMemEval. Not a tuned, full-dataset result.

### Pipeline smoke (not a benchmark result)

A 2-question hand-written LoCoMo-shaped fixture (`samples/locomo-smoke.json`), graded by
`codex` gpt-5.5 at `low` reasoning, validates the full load → ACC → answer → grade path:

```
dataset:             locomo
cases:               2
graded:              2
accuracy:            1.000
mean tokens/query:   59.0
footprint_ratio:     0.797
```

This only proves the harness runs end to end; real numbers come from the full datasets above.
