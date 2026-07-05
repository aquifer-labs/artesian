---
type: reference
title: Reciprocal Rank Fusion (RRF)
description: Rank-based fusion of keyword and vector search results.
tags: [retrieval, hybrid, rrf]
timestamp: 2026-06-14T00:00:00Z
node_id: node:rrf
tier: l2-scenario
---

# Reciprocal Rank Fusion

Artesian fuses the keyword channel and the vector channel by rank, not by raw score, so a BM25
score and a cosine score combine without normalization. For a document `d` at 1-based rank
`r_c(d)` in channel `c`:

`RRF(d) = sum over channels c of 1 / (k + r_c(d))`, with `k = 60`.

See [Embedding Model](embedding-model.md) for the vector channel. Implemented in
`crates/aquifer/src/rrf.rs`.

# Citations

1. Cormack, Clarke & Büttcher, *Reciprocal Rank Fusion* (SIGIR 2009).
