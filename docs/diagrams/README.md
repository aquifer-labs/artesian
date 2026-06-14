<!-- SPDX-License-Identifier: Apache-2.0 -->

# Diagrams

**Mermaid is the source of truth.** Inline ` ```mermaid ` blocks in the docs render natively on
GitHub, diff cleanly, and stay editable. For a few **hero** diagrams (README + architecture
overview) we additionally render polished PNGs with a shared brand theme. If a hero's inline
Mermaid and its `.mmd` source ever differ, the `.mmd` source wins.

## Layout

```
docs/diagrams/
├── mermaid-config.json     # shared theme (themeVariables) for mmdc + inline init
├── *.mmd                   # hero diagram sources (source of truth)
└── *.png                   # rendered output (committed, embedded in README/architecture)
```

Hero diagrams (keep this set small — only top-level overview pictures):

- `system-map` — the whole-system map (used in `README.md` and `docs/architecture.md`).
- `memory-overview` — short vs long-term memory flow (`docs/memory.md` §1).
- `orchestrator-blackboard` — master/worker/judge via the shared blackboard (`docs/orchestration.md`).

Everything else stays inline mermaid in its doc — do **not** PNG-render the long tail.

## Rendering (mermaid-cli / mmdc)

```bash
# one-off (no global install)
npx -y @mermaid-js/mermaid-cli \
  -i docs/diagrams/system-map.mmd -o docs/diagrams/system-map.png \
  -c docs/diagrams/mermaid-config.json -b transparent -s 2
```

Or the repo target (added by tooling): `just diagrams` renders every `docs/diagrams/*.mmd` to a
matching `*.png` with the shared config at scale 2. Re-render and commit PNGs **locally** after
changing a `.mmd` (run `just diagrams`). CI does **not** re-render or byte-diff PNGs — cross-OS
renders are not byte-reproducible, so that would fail spuriously; the `.mmd` source is
authoritative and the committed PNG is a convenience artifact.

## Embedding

In a hero location, embed the PNG and keep the mermaid source one click away:

```markdown
[![System map](docs/diagrams/system-map.png)](docs/diagrams/system-map.mmd)
```

## Style

The shared theme (see `mermaid-config.json`) uses a light, layered, ClickHouse-style palette:
soft fills, defined strokes, a clean sans-serif, generous spacing. Node colors follow the same
class palette used inline (agents indigo, surfaces cyan, core amber, capabilities green, stores
slate, judge red). Keep diagrams shallow and grouped with subgraphs; prefer a few labeled edges
over many crossing ones.
