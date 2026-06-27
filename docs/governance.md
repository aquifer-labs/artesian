<!-- SPDX-License-Identifier: Apache-2.0 -->

# Governance — nothing is lost

> **Every commit, eviction, decay, and dream is logged in the OCF qualify trail.
> Forgetting is as auditable as remembering.**

This is one of Artesian's core differentiators. Most agent memory systems silently discard
context when the window fills — the agent "forgets" a fact with no record of what was lost
or why. Artesian treats eviction and compression as first-class governance events, logged
permanently in a machine-readable audit trail.

## The qualify trail

Every memory lifecycle event writes an entry to `qualify.jsonl` inside the collection's
OCF bundle:

```
~/.artesian/<collection>/qualify.jsonl
```

Each line is a JSON event with a type, timestamp, and the affected record's content hash:

```jsonc
// commit: record accepted into the Committed Context State
{
  "event": "commit",
  "ts": "2026-06-18T14:02:11Z",
  "record_id": "r:abc123",
  "scores": { "novelty": 0.87, "relevance": 0.93, "drift": 0.04 },
  "gate": "accepted"
}

// evict: record removed under saturation (content preserved in durable store)
{
  "event": "evict",
  "ts": "2026-06-18T14:07:33Z",
  "record_id": "r:def456",
  "reason": "footprint_saturation",
  "gate": "evicted"
}

// dream: offline consolidation run (dream-on-compact)
{
  "event": "dream",
  "ts": "2026-06-18T15:00:01Z",
  "source": "compaction_hook",
  "records_consolidated": 14,
  "records_merged": 3,
  "output_path": "~/.artesian/dreams/2026-06-18T150001Z/"
}

// decay: record marked low-relevance by the consolidation pass (planned)
{
  "event": "decay",
  "ts": "2026-06-18T15:00:04Z",
  "record_id": "r:ghi789",
  "reason": "redundancy",
  "gate": "decayed"
}
```

The `gate` field is the authoritative verdict. Accepted records are in the CCS. Evicted and
decayed records are still queryable in the durable store — they just no longer occupy the
bounded committed context. The qualify trail is the bridge between "what the agent is
using right now" and "what was ever stored."

## Qualify-gate: what enters the CCS

The qualify-gate sits between the recall channel and the Committed Context State:

```
recall candidates  ──►  qualify-gate  ──►  CCS (bounded)
                          │
                          ▼
                     qualify.jsonl  (every decision logged)
```

The gate evaluates three signals for each recall candidate:

| Signal | What it measures | Default threshold |
|---|---|---|
| **Novelty** | Is this record new information vs what is already in the CCS? | > 0.15 cosine distance from current CCS centroid |
| **Relevance** | Does this record address the current task / query? | > 0.65 cosine similarity to query embedding |
| **Drift** | Is the record consistent with the CCS, or does it contradict it? | < 0.30 drift score |

Records that pass all three gates are admitted (`gate: accepted`). Records that fail are
logged as rejected with the reason. No silent drops.

### Callable qualify gate

The same gate can be called before storing or injecting content:

```shell
artesian qualify "candidate memory text" --goal "current task" --json
```

MCP clients can call `memory.qualify` with `candidate` and optional `goal`. Both surfaces return
`admitted`, `reason`, `signals`, `agreement`, `chance_corrected_agreement`, and `confidence`. This
is intended for PreToolUse-style hooks: reject low-confidence or low-agreement candidates before
they enter the working context, while preserving the audited signal trail for review.

## Dream-on-compact: offline consolidation

When `dream_on_compact = true` is set in `artesian.toml`, Artesian spawns a fully detached
background process on every auto-compaction. This process:

1. Reads the current durable store (never mutates the live store)
2. Runs consolidation: redundancy collapse, entity resolution, atomic-fact distillation
3. Writes output to `~/.artesian/dreams/<timestamp>/` with its own `qualify.jsonl`
4. Exits; the result is advisory (operator can inspect before importing)

The dream process's qualify trail is a complete record of what was merged, collapsed, or
discarded during consolidation.

See [self-repair docs](self-repair.md) for the full `dream_on_compact` configuration.

## Replay and verification

Because the qualify trail is append-only JSONL, you can replay any past state:

```shell
# View all events for a collection
artesian okf qualify --collection default

# Show only evictions in the last 7 days
artesian okf qualify --collection default --event evict --since 7d

# Verify that the current CCS matches the expected state
artesian okf verify
```

The `artesian okf verify` command replays the qualify trail and asserts that the current
durable store is consistent with the logged history. Use it after any migration or restore.

## OCF and interoperability

The qualify trail is part of the [Open Committed-state Format (OCF)](ocf.md), the portable
schema that governs Artesian's kit bundles and session handoffs. Because OCF is an open spec,
any tool — not just Artesian — can read, parse, and audit the qualify trail.

See [OCF specification](ocf.md) and [kit format](kit-format.md).

## Summary

| Event | Logged? | Record preserved in durable store? |
|---|---|---|
| Commit (record enters CCS) | ✓ | ✓ |
| Eviction (CCS saturation) | ✓ | ✓ (still queryable) |
| Rejection (gate failed) | ✓ | ✓ |
| Dream consolidation merge | ✓ | ✓ (in dream output dir) |
| Decay / redundancy mark | ✓ (planned) | ✓ |

Nothing is silently lost. Every governance decision is machine-readable, replayable, and
portable via OCF.
