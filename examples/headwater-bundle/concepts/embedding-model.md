---
type: reference
title: Embedding Model
description: The pinned multilingual embedder used for the vector channel.
tags: [embeddings, e5, multilingual]
timestamp: 2026-06-14T00:00:00Z
node_id: node:embedding-model
tier: l3-project
---

# Embedding Model

Artesian pins one model across every workstation and backend so vectors are comparable:
`intfloat/multilingual-e5-small`, 384 dimensions, cosine distance. Queries are embedded with the
`query:` prefix and stored passages with `passage:` (E5 convention).

Used by the vector channel that [RRF](rrf.md) fuses with keyword search. Implemented in
`crates/aquifer/src/vector_memory.rs`.

# Citations

1. Wang et al., *Multilingual E5 Text Embeddings* (2024). https://arxiv.org/abs/2402.05672
