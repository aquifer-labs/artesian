---
type: Index
title: Artesian Memory — Example Headwater Bundle
description: A tiny, self-describing headwater bundle used as a docs example and test fixture.
tags: [example, headwater, memory]
timestamp: 2026-06-14T00:00:00Z
headwater_version: "0.1"
---

# Artesian Example Headwater Bundle

This directory is a headwater bundle: plain markdown files with YAML frontmatter, no vector database
required. Its markdown shape aligns with Google's
[Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog). Artesian's
`files` memory backend reads and writes bundles in exactly this shape (see `docs/memory.md` §4.1).

Concepts:

- [Reciprocal Rank Fusion](concepts/rrf.md) — how hybrid retrieval is fused.
- [Embedding Model](concepts/embedding-model.md) — the pinned multilingual embedder.

Update history lives in [log.md](log.md).

> Frontmatter requires only `type`. `title`/`description`/`tags`/`timestamp` are recommended;
> Artesian adds `node_id` and `tier` as tolerated extensions. Relationships are plain markdown links.
