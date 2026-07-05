<!-- SPDX-License-Identifier: Apache-2.0 -->

# Upgrades — Evolving Artesian Without Losing Memory

Artesian treats the headwater markdown bundle as the durable record of truth. Vector databases are derived
indexes. If an embedding model, dimension, distance metric, or payload schema changes, rebuild the
index from headwater files instead of mixing vectors in place.

## Compatibility Guard

Each vector collection stores Artesian compatibility metadata:

- Artesian crate version
- headwater version
- embedding model id
- embedding dimensions
- distance metric

`VectorMemoryBackend` refuses reads and writes when the collection metadata does not match the
configured model or dimension. This prevents silent vector mixing. A mismatch means: migrate first.

## Safe Migration Procedure

1. Verify the headwater bundle:

   ```sh
   artesian headwater verify .artesian/memory
   ```

2. Rebuild into a new collection under the current model/dimension/schema:

   ```sh
   artesian migrate headwater-bundle .artesian/memory --config artesian.toml --retention-days 30
   ```

3. Artesian backfills by content hash, so re-running the command resumes and skips duplicates.

4. When the new collection verifies, Artesian swaps the Qdrant alias to the new collection and keeps
   the old collection for the configured retention window. Rollback is then an alias swap back to
   the retained collection.

5. Keep the old collection for at least one backup cycle before deleting it manually.

Before deleting any Qdrant collection by hand, inspect `GET /collections/aliases` and confirm no
logical collection name still points at that physical collection. Normal runtime open/init uses the
configured collection name literally; alias swaps are only part of the Qdrant migration flow.

For deployments that use `artesian migrate headwater-bundle`, configure `memory.collection` as a Qdrant
alias. Atomic alias swap requires an alias; it cannot safely shadow a concrete collection with the
same name.

## Snapshots and Headwater Exports

Use both backups:

- Qdrant snapshot for fast restore of the derived index.
- headwater export for the durable source of truth.

Example schedule:

```sh
# daily index snapshot
artesian snapshot --config artesian.toml --output-dir /path/to/qdrant-snapshots

# daily source-of-truth export
artesian headwater export .artesian/memory /path/to/headwater-backups/$(date +%Y-%m-%d)
artesian headwater verify /path/to/headwater-backups/$(date +%Y-%m-%d)
```

Keep backup paths environment-specific and out of the repository.
On default ports, one Qdrant URL is enough: `:6333` derives gRPC `:6334`, and `:6334` derives REST
`:6333`. If your Qdrant REST API is not the default sibling of the configured gRPC endpoint, set
`qdrant_rest_url` in `artesian.toml` or `QDRANT_REST_URL` for snapshot download and alias swap.

## Recovery Pattern

1. Restore or copy the headwater bundle.
2. Run `artesian headwater verify <restored-headwater-dir>`.
3. Run `artesian migrate headwater-bundle <restored-headwater-dir>` to rebuild the current vector index.
4. Point the alias at the rebuilt collection.
5. Keep the old collection until the new run has been verified in normal use.
