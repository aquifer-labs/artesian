<!-- SPDX-License-Identifier: Apache-2.0 -->

# Brunnr Documentation

Brunnr is a multi-agent context orchestration system: pluggable agent **memory**, optional
**master/worker/judge** orchestration, and optional task tracking — all non-intrusive, so you
keep driving your agent the way you already do and simply gain speed and lower token cost.

Start here, then read the topic you need:

| Doc | What it covers |
|---|---|
| [architecture.md](architecture.md) | Top-level system architecture and crates |
| [modes.md](modes.md) | Operating modes: `memory`, `orchestrate`, `full`, `advanced` |
| [memory.md](memory.md) | Mímisbrunnr memory: short/long-term, retrieval math, tiers |
| [backends.md](backends.md) | Memory backends (Files, sqlite-vec, Qdrant) and RRF per backend |
| [task-tracking.md](task-tracking.md) | Þing task tracker: md/vector/external (Jira, Linear) |
| [self-repair.md](self-repair.md) | Surviving context auto-compaction |
| [yggdrasil.md](yggdrasil.md) | Layered, priority-ordered context-md tree |
| [development.md](development.md) | Build, test, and contribution workflow |

Naming is Norse (Mímisbrunnr = memory, Urðarbrunnr = roles, Þing = tasks, Óðinn/Þórr/Týr =
master/worker/judge); plain-English aliases exist everywhere it matters.
