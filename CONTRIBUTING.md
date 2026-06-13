<!-- SPDX-License-Identifier: Apache-2.0 -->

# Contributing

Brunnr welcomes focused contributions that keep the project portable, testable, and easy to inspect.

## Developer Certificate of Origin

This project uses the Developer Certificate of Origin. Every commit must include a sign-off:

```shell
git commit -s
```

The sign-off certifies that you have the right to submit the contribution under Apache-2.0.

## Before Sending Changes

Run:

```shell
cargo fmt --all --check
cargo test --workspace
cargo build --workspace
```

Keep pull requests scoped to one behavior or documentation change. Include tests for implemented code.
