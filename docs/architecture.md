<!-- SPDX-License-Identifier: Apache-2.0 -->

# Architecture

Brunnr is a Cargo workspace with strict crate boundaries.

`brunnr-core` owns orchestration-neutral primitives: roles, task queue types, config, and the `Agent` adapter trait. It does not know how memory is stored and does not own process-specific adapter implementations.

`mimisbrunnr` owns memory contracts and backends. `FilesBackend` is the zero-infrastructure backend. Qdrant, sqlite-vec, and TencentDB fit behind the same `MemoryBackend` trait.

`brunnr-mcp` exposes memory tools to agents. The initial tools are `memory.find` and `memory.store`.

`brunnr-cli` is the user-facing entrypoint for initialization, memory checks, and role spawn requests.
