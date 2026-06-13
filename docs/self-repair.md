<!-- SPDX-License-Identifier: Apache-2.0 -->

# Self Repair

Brunnr's self-repair model is based on deterministic anchors plus targeted recall.

The future session anchor should record:

- current task;
- active plan pointer;
- last important decisions;
- next concrete step.

On resume or compaction, an agent can read the anchor and call memory search for only the relevant supporting records.
