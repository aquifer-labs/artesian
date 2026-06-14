<!-- SPDX-License-Identifier: Apache-2.0 -->

# AGENTS.md

These instructions apply to the entire repository.

## Onboarding (humans and agents)

To bring Brunnr up — deploy it and connect a project per the operator's requested config —
follow [docs/onboarding.md](docs/onboarding.md). It has a human Quickstart and a deterministic,
idempotent, non-destructive **AI-agent recipe** (collect mode/backend/qdrant/project, run
`brunnr init`, backfill, verify, report) with hard guardrails: never delete or overwrite existing
memory or unrelated config, keep secrets out of git, never switch an existing collection's
embedding model in place (use `migrate`), and never push or take outward-facing actions without
explicit operator approval. New here? read [docs/positioning.md](docs/positioning.md) and
[docs/README.md](docs/README.md) first.

## Language

All code, documentation, commit messages, plans, and handoff notes in this repository must be written in English.

## Boundaries

- Keep the repository universal and anonymized.
- Do not commit secrets, machine-local paths, private infrastructure names, runtime logs, or generated build output.
- Keep crate boundaries strict: orchestration primitives in `brunnr-core`, memory in `mimisbrunnr`, MCP in `brunnr-mcp`, CLI in `brunnr-cli`.
- Add SPDX license headers to source, docs, manifests, and workflow files.
- Do not push unless a maintainer explicitly asks.

## Rust

- Use stable Rust pinned by CI.
- Run `just ci` before marking work complete. If `just` is unavailable, run the equivalent cargo commands in `docs/development.md`.
- Prefer small modules and explicit public exports.
- Newly added traits must document their contract.
- Write tests for every implemented behavior.

## Contribution Hygiene

All commits must use DCO sign-off:

```text
Signed-off-by: Your Name <you@example.com>
```

Keep changes focused. Do not mix unrelated refactors into feature work.
