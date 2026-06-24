<!-- SPDX-License-Identifier: Apache-2.0 -->

# OCF — Open Committed-state Format

The **Open Committed-state Format (OCF)** is the portable, schema-governed exchange format
that underlies Artesian's working-context kits, session handoffs, and governance qualify trail.

OCF is defined as an open specification so that any tool — not just Artesian — can read,
write, verify, and audit agent memory.

**Specification repository:** [github.com/aquifer-labs/ocf](https://github.com/aquifer-labs/ocf)

---

## What OCF provides

| Capability | Description |
|---|---|
| **Portable kit bundles** | A working-context snapshot any agent can import, regardless of backend |
| **Qualify trail** | Append-only governance log: commits, evictions, decays, consolidations |
| **Schema governance** | Versioned JSON schemas for all record types — validated on import |
| **Session handoff** | Structured packet for handing off between Claude Code, Codex, Gemini, etc. |
| **Audit & replay** | Replay any past CCS state from the qualify trail |

---

## Bundle layout

An OCF bundle is a directory (or `.tar.gz` archive) with four files:

```
artesian.working-context/
├── manifest.json       # format version, collection, timestamps, schema refs
├── snapshot.json       # current CCS records (validated against schema)
├── lifecycle.jsonl     # ordered qualify-trail events
└── snapshot.md         # human-readable summary (OKF markdown)
```

### `manifest.json`

```jsonc
{
  "format": "artesian.working-context",
  "version": "0.1",
  "collection": "my-project",
  "exported_at": "2026-06-18T14:00:00Z",
  "agent_id": "claude-code",
  "schema": "https://github.com/aquifer-labs/ocf/blob/main/schema/v0.1/snapshot.schema.json"
}
```

### `snapshot.json`

Array of committed memory records at the time of export. Each record follows the
`MemoryRecord` schema: `id`, `content`, `tier` (L0–L3), `tags`, `created_at`,
`embedding_model`, `scores`.

### `lifecycle.jsonl`

The [qualify trail](governance.md) for this bundle's history — one JSON event per line.
Events: `commit`, `evict`, `reject`, `dream`, `decay`, `import`.

### `snapshot.md`

Human-readable OKF markdown: one section per record tier, with content summaries.
Readable without any Artesian tooling.

---

## Using OCF with Artesian CLI

```shell
# Export the current working context as an OCF bundle
artesian kit export --out my-project.wc.tar.gz

# Import a bundle into the current collection
artesian kit import my-project.wc.tar.gz

# Verify bundle integrity and schema conformance
artesian okf verify

# Inspect the qualify trail
artesian okf qualify
```

MCP tools: `memory.kit.export`, `memory.kit.import`, `memory.kit.status`.

---

## Using OCF without Artesian

Because the format is open, you can inspect and process OCF bundles with any JSON/JSONL tool:

```shell
# Unpack a kit bundle
tar -xzf my-project.wc.tar.gz

# Read the qualify trail
cat artesian.working-context/lifecycle.jsonl | jq 'select(.event == "evict")'

# Count records by tier
cat artesian.working-context/snapshot.json | jq 'group_by(.tier) | map({tier: .[0].tier, count: length})'

# Human-readable summary
cat artesian.working-context/snapshot.md
```

---

## Relationship to OKF

Artesian's `files` backend aligns with **OKF (Open Knowledge Format)** — Google Cloud
`knowledge-catalog` (Apache-2.0). OKF defines the human-readable markdown+YAML format
for individual memory records. OCF extends OKF with:

- Versioned bundle manifests
- A structured qualify trail (governance log)
- Schema-validated snapshots
- Session handoff semantics

---

## OCF spec

The canonical specification, JSON schemas, and reference validator live at:

**[github.com/aquifer-labs/ocf](https://github.com/aquifer-labs/ocf)**

OCF is Apache-2.0 licensed and governed by [Aquifer Labs](https://github.com/aquifer-labs).
Contributions and implementation feedback are welcome via GitHub issues.

---

## Further reading

- [Governance — nothing is lost](governance.md): how the qualify trail is used
- [Kit format](kit-format.md): detailed field schemas for bundle contents
- [Self-repair](self-repair.md): how OCF kits survive auto-compaction
- [Concurrency](concurrency.md): session handoff between concurrent agents
