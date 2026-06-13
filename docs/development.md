<!-- SPDX-License-Identifier: Apache-2.0 -->

# Development

Brunnr follows a small-workspace version of the structure used by mature Rust repositories such
as OpenAI Codex, Zed, Polars, and Ruff: domain crates under `crates/`, shared workspace
dependencies, crate-level integration tests, and repo-level command aliases.

## Layout

- Production code lives under `crates/<crate>/src`.
- Public contract and integration tests live under `crates/<crate>/tests`.
- Shared test-only utilities live in `crates/brunnr-test-support` and are never published.
- Future fixture-heavy tests should use `tests/fixtures` or `test_data` inside the owning crate.
- Repository-level tooling lives in `.cargo/`, `.config/`, `rustfmt.toml`, `clippy.toml`, and
  `justfile`.

## Commands

```shell
just ci
```

Equivalent cargo aliases:

```shell
cargo brunnr-clippy
cargo brunnr-test
cargo brunnr-build
```

Use `cargo test --workspace` to verify default features and
`cargo test --workspace --all-features` to verify optional backends such as Qdrant.
