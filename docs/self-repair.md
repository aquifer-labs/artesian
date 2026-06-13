<!-- SPDX-License-Identifier: Apache-2.0 -->

# Self-Repair Across Auto-Compaction

Long agent sessions hit context auto-compaction: the host summarizes or truncates the context and
the agent can lose its place. Brunnr makes this a non-event by combining a deterministic anchor
with targeted recall, so even switching agents mid-task (e.g. Claude Code → Codex) is lossless.

Three mechanisms:

1. **Session anchor (Muninn)** — a tiny, always-current record of the in-flight task, the active
   plan pointer, the last N decisions, and the next concrete step. Cheap to update every turn.
2. **Continuous externalization** — durable learnings are written to long-term memory as they
   occur, so truncation loses nothing recoverable via `memory.find`.
3. **Self-repair hook** — on a detected compaction/resume boundary the agent re-reads the anchor
   (deterministic — answers "what is my current step") and runs a targeted `memory.find`
   (semantic — restores surrounding knowledge) before its next action. No manual "re-read the
   docs" step.

```mermaid
flowchart LR
  C{Compaction / resume?} -->|yes| RA[Read session anchor]
  RA --> RC["memory.find(current task) — top-k"]
  RC --> ACT[Resume exact next step]
  C -->|no| ACT
```

Status: the anchor schema and the recall path are designed here; wiring the detection hook and
the `Muninn` writer is a planned implementation step. See [memory.md](memory.md) §6.
