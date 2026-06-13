<!-- SPDX-License-Identifier: Apache-2.0 -->

# CLAUDE.md

Brunnr is an English-only open-source Rust workspace. Keep all in-repository artifacts universal and free of local operator details.

## Project Shape

The first milestone is a working `memory` mode:

1. `mimisbrunnr::MemoryBackend` is the stable memory seam.
2. `FilesBackend` provides zero-infrastructure markdown storage.
3. `brunnr-mcp` exposes `memory.find` and `memory.store`.
4. `brunnr-cli` provides `init`, `memory store`, `memory find`, and role-aware `spawn`.

## Validation

Run:

```shell
cargo fmt --all --check
cargo test --workspace
cargo build --workspace
```

Do not push unless explicitly asked.
