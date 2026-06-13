<!-- SPDX-License-Identifier: Apache-2.0 -->

# AGENTS.md

These instructions apply to the entire repository.

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
- Run `cargo fmt --all --check`, `cargo test --workspace`, and `cargo build --workspace` before marking work complete.
- Prefer small modules and explicit public exports.
- Newly added traits must document their contract.
- Write tests for every implemented behavior.

## Contribution Hygiene

All commits must use DCO sign-off:

```text
Signed-off-by: Your Name <you@example.com>
```

Keep changes focused. Do not mix unrelated refactors into feature work.
