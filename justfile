# SPDX-License-Identifier: Apache-2.0

set shell := ["sh", "-eu", "-c"]

default:
    just --list

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p mimisbrunnr --features qdrant --all-targets -- -D warnings

test:
    cargo test --workspace
    cargo test --workspace --all-features

build:
    cargo build --workspace
    cargo build --workspace --all-features

ci: fmt clippy test build
