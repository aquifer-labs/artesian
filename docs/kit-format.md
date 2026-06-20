<!-- SPDX-License-Identifier: Apache-2.0 -->

# Working-Context Bundle — format

> A portable snapshot of an agent's **committed working context** plus its **lifecycle** — so an
> agent (any model, any runtime) can *resume* work, not just retrieve facts. This is **not** a
> memory archive; it composes *with* the memory-unit formats that already exist.

## The gap this fills

Every portable agent-memory format standardizes the memory **unit** (facts / records): Portable AI
Memory, AMP, OAMP, content-addressed grains, plain files. None of them standardizes the layer that
actually lets an agent pick up where another left off: **what the agent holds in force right now**
(a bounded, typed working set) and **how it got there** (a declarative commit lifecycle).

So this format owns only that missing layer, and *references* the unit layer instead of redefining
it. A bundle is small, human-readable, and composable.

## Bundle layout

A bundle is a directory:

```
<bundle>/
  manifest.json      # what this is + which unit layer it composes with
  snapshot.json      # the bounded, typed committed working state
  lifecycle.jsonl    # append-only log of commit / supersede / deprecate decisions
  snapshot.md        # human-readable mirror of snapshot.json (interface ≠ substrate)
```

`manifest.json` and `snapshot.json` are required; `lifecycle.jsonl` and `snapshot.md` are optional.

### manifest.json

| field | type | notes |
|---|---|---|
| `format` | string | always `artesian.working-context` |
| `version` | string | `0.1` — readers accept the same major version |
| `agent_id` | string? | optional producer id |
| `created_at` | RFC3339 | |
| `unit_source` | string | `inline` (entries carry content) or the unit layer this composes with: `pam`, `amp`, `files`, … |
| `unit_ref` | string? | pointer/URI to the external unit store when `unit_source != inline` |

### snapshot.json — the committed working state

A **bounded** (`budget_tokens`), **typed** (`schema` slots) set of entries — the ACC committed
context state, made portable.

| field | type | notes |
|---|---|---|
| `schema` | string[] | slot names, in render order (e.g. `decision`, `constraint`, `fact`, `task-state`) |
| `budget_tokens` | int | the saturation bound |
| `token_count` | int | current footprint; must equal the sum of entry tokens |
| `entries[]` | object | the committed entries |

Each entry:

| field | type | notes |
|---|---|---|
| `id` | string | stable within the bundle |
| `slot` | string | one of `schema` |
| `content` | string | may be empty when `resolution = pointer` |
| `tokens` | int | |
| `score` | float | committed value (drives eviction) |
| `resolution` | enum | `full` \| `compressed` \| `pointer` — how the content is currently represented |
| `unit_ref` | string? | reference into the unit layer, when composed |
| `committed_at` | RFC3339 | |

### lifecycle.jsonl — what is in force, and why

One JSON object per line, appended in order. This is the part that travels *with* the data so an
importer trusts the right thing:

| field | type | notes |
|---|---|---|
| `ts` | RFC3339 | |
| `entry_id` | string | references a snapshot entry |
| `decision` | enum | `commit` \| `evict` \| `supersede` \| `deprecate` |
| `status` | enum | `hypothesis` \| `active` \| `validated` \| `deprecated` \| `superseded` |
| `supersedes` | string? | the entry id this one replaces |
| `reason` | object? | qualify-gate signals: `{ relevance, novelty, drift }` |

## Tiny example

`snapshot.json` (one entry shown):

```json
{
  "schema": ["decision", "constraint", "fact", "task-state"],
  "budget_tokens": 4096,
  "token_count": 7,
  "entries": [
    {
      "id": "a", "slot": "decision",
      "content": "ship the working-context bundle first",
      "tokens": 7, "score": 1.0, "resolution": "full",
      "committed_at": "2026-06-21T08:30:00Z"
    }
  ]
}
```

`lifecycle.jsonl`:

```json
{"ts":"2026-06-21T08:30:00Z","entry_id":"a","decision":"commit","status":"active","reason":{"relevance":0.9,"novelty":0.5,"drift":0.0}}
```

## Reference implementation

Artesian implements this layer in the `headgate` crate
([`WorkingContextBundle`](../crates/headgate/src/bundle.rs)) and exposes it on the CLI:

```bash
artesian kit export --format bundle --output ./wc        # write a bundle from the loop kit
artesian kit import ./wc                                  # validate + print the resumable context
```

A live ACC session can produce a richer snapshot directly from its committed state via
`WorkingContextSnapshot::from_ccs`.

## Composes with

- **Unit layer** — reference Portable AI Memory / AMP / files / any store via `unit_source` +
  per-entry `unit_ref`; this bundle does not redefine the unit.
- **Encryption / compliance** — the bundle is a payload; it can sit inside an encrypted memory-cell
  envelope without change.

## Status

`v0.1`, internal to Artesian, deliberately minimal and name-agnostic so the layer can be promoted to
a standalone open specification without code churn.
