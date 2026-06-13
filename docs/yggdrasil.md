<!-- SPDX-License-Identifier: Apache-2.0 -->

# Yggdrasil

Yggdrasil is Brunnr's layered context tree.

The model is intentionally simple: high-priority root documents describe project-wide policy, while deeper package-level documents add local context. Retrieval should assemble a bounded slice instead of replaying an entire repository memory dump.

Memory records include `node_id` so summarized context can drill down to the source record when evidence is needed.
