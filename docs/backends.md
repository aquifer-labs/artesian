<!-- SPDX-License-Identifier: Apache-2.0 -->

# Backends

`MemoryBackend` defines four required seams:

- `store`: persist durable memory.
- `find`: retrieve relevant memory.
- `hybrid_rrf`: fuse multiple retrieval channels with reciprocal rank fusion.
- `get_node`: drill down by deterministic `node_id`.

## FilesBackend

The files backend stores date-tagged markdown records under `.brunnr/memory/YYYY-MM-DD/<id>.md`.

## QdrantBackend

The Qdrant seam is feature-gated as `mimisbrunnr/qdrant`. It pins `intfloat/multilingual-e5-small` with 384 dimensions for the first shared-vector default.

## Future Backends

`SqliteVecBackend` and `TencentDBBackend` are reserved names behind the same trait.
