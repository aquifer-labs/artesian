# SPDX-License-Identifier: Apache-2.0

set shell := ["sh", "-eu", "-c"]

default:
    just --list

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace
    cargo test --workspace --all-features

build:
    cargo build --workspace
    cargo build --workspace --all-features

diagrams:
    for source in docs/diagrams/*.mmd; do \
        output="${source%.mmd}.png"; \
        npx -y @mermaid-js/mermaid-cli \
            -i "$source" \
            -o "$output" \
            -c docs/diagrams/mermaid-config.json \
            -b transparent \
            -s 2; \
    done

bench:
    cargo run -p artesian-bench -- --reps 2 --signal-arms

bench-large:
    cargo run -p artesian-bench -- --reps 2 --seed-corpus benchmarks/large-corpus --results benchmarks/results/large-corpus --signal-arms

bench-large-source:
    python3 benchmarks/tools/generate_large_source_corpus.py
    cargo run -p artesian-bench -- --reps 2 --seed-corpus benchmarks/large-source-corpus --results benchmarks/results/large-source-run --signal-arms

bench-xl:
    python3 benchmarks/tools/generate_corpus.py --out xl-corpus --docs 180 --tasks 40
    cargo run -p artesian-bench -- --reps 2 --seed-corpus benchmarks/xl-corpus --results benchmarks/results/xl-corpus

bench-session:
    python3 benchmarks/tools/generate_corpus.py --out session-corpus --docs 1600 --tasks 60
    cargo run -p artesian-bench -- --reps 1 --seed-corpus benchmarks/session-corpus --results benchmarks/results/session-corpus

bench-mid:
    python3 benchmarks/tools/generate_corpus.py --out mid-corpus --docs 6400 --tasks 80
    cargo run -p artesian-bench -- --reps 1 --seed-corpus benchmarks/mid-corpus --results benchmarks/results/mid-corpus

bench-mega:
    python3 benchmarks/tools/generate_corpus.py --out mega-corpus --docs 14000 --tasks 100
    cargo run -p artesian-bench -- --reps 1 --seed-corpus benchmarks/mega-corpus --results benchmarks/results/mega-corpus

bench-plot:
    python3 benchmarks/tools/plot_scaling.py

bench-check:
    just bench
    just bench-large
    just bench-xl
    just bench-session
    just bench-mid
    just bench-mega
    just bench-large-source
    git diff --exit-code -- benchmarks/results/sample-run benchmarks/results/large-corpus benchmarks/results/xl-corpus benchmarks/results/session-corpus benchmarks/results/mid-corpus benchmarks/results/mega-corpus benchmarks/results/large-source-run

ci: fmt clippy test build
