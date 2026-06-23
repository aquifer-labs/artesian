# Token Savings

Artesian records how many tokens its targeted recall operations save versus loading the full
source records, so you can see the footprint benefit at any time.

## Quick start

```sh
# Human-readable one-liner
artesian tokens

# JSON rollup (machine-readable / badge use)
artesian tokens --json

# Per-operation breakdown
artesian tokens --by-op

# Filter to the last 30 days
artesian tokens --since 2026-06-01T00:00:00Z
```

The same data is available from the MCP server:

```json
// tool call
{ "name": "memory.savings", "arguments": {} }

// with a time filter
{ "name": "memory.savings", "arguments": { "since": "2026-06-01T00:00:00Z" } }
```

## Baseline assumption (read this before citing numbers)

> **`baseline_tokens`** is the sum of `count_tokens(record.content)` for every **unique source
> record** that contributed a hit in the recall response — i.e. "you received a bounded slice
> instead of loading those whole records."

This is a **conservative, per-operation** baseline.  It counts only the records that were
actually retrieved, not the entire memory corpus.

### Per-operation breakdown

| Operation | `baseline_tokens` | `returned_tokens` | Typical saving |
|-----------|------------------|-------------------|----------------|
| `loop.recall` | Full content of each MMR-selected hit (up to `LOOP_RECALL_LIMIT` records × full content) | 280-char truncated lines joined with newlines | **Significant** when records are long prose paragraphs |
| `memory.context` | Full `index.md` content + full hit record content | Truncated index slice (`index_chars`, default 4 000 chars) + hit content | **Meaningful** when `index.md` grows beyond the limit |
| `memory.find` | Full content of returned hits | Same (full content is returned) | ~0 — no truncation happens here |
| `memory.session.resume` / `resume_by_task` | Resume packet tokens | Same | ~0 — the full packet is returned |

`saved_tokens = max(0, baseline_tokens − returned_tokens)` — never negative.

## Where data is stored

| File | Contents |
|------|----------|
| `~/.artesian/token_savings.jsonl` | Append-only log; one JSON line per measured recall |
| `~/.artesian/token_savings.json` | Compact rollup updated on every write (fast CLI reads) |

Override the directory with the `ARTESIAN_STATS_DIR` environment variable.

## Disabling stats collection

Set `track_savings = false` in the `[memory]` section of `artesian.toml`:

```toml
[memory]
track_savings = false
```

Stats collection is best-effort: any I/O failure is silently swallowed and never propagates to
the recall operation itself.

## Config reference

| Field | Default | Description |
|-------|---------|-------------|
| `memory.track_savings` | `true` | Enable/disable token-savings recording |
| `ARTESIAN_STATS_DIR` (env) | `~/.artesian` | Override the stats directory |

## Tokenizer

The same `cl100k_base` tokenizer used by the benchmark suite (`headgate::count_tokens`).
Falls back to `chars / 4` if the tokenizer cannot be loaded (no network required; bundled).
