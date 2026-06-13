<!-- SPDX-License-Identifier: Apache-2.0 -->

# Security Policy

Brunnr is early-stage software. Please report suspected security issues privately instead of opening a public issue.

## Supported Versions

No stable release is supported yet. Security fixes land on `main` until the first release line exists.

## Reporting a Vulnerability

Send a private report to the maintainers with:

- affected component;
- reproduction steps;
- expected impact;
- whether any secret or private data is involved.

Do not include real secrets, private hostnames, or private infrastructure identifiers in reports.

## Security Design Notes

- Secrets must never be committed.
- MCP tools should expose narrow capabilities.
- Sandbox support is optional and must be explicit.
- Memory backends must preserve project isolation and deterministic drill-down.
