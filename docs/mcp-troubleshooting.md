<!-- SPDX-License-Identifier: Apache-2.0 -->

# MCP Troubleshooting — "Connected" but Tools Don't Work

## Symptom

An agent (Claude Code, most often) reports the `artesian-memory` MCP server as **Connected**. Its
system-prompt instructions ("ALWAYS search the project memory before non-trivial work...") are
visible. But the agent never actually calls `memory.find` / `memory.store` — it falls back to
re-reading files with the CLI instead, or apologizes that it "doesn't have that tool available."
No error is printed anywhere in the transcript.

## Root Cause

This is an **upstream Claude Code resume limitation**, not an artesian-mcp defect.

When the harness restarts its MCP connections mid-session — a Claude Code self-update, an
MCP/connector config change, or anything else that makes the harness re-negotiate servers — the
session's tool list is updated with a `deferred_tools_delta` event that *removes* the
`mcp__artesian-memory__*` tool names. If you then `--resume` that session, Claude Code re-sends the
server's instructions (`mcp_instructions_delta` adds them back — that is why the prompt still
mentions memory), but it does **not** re-register the callable tools. The session is left in a
state where the model believes the server is available (it can see the instructions) but has no
way to invoke it.

Verified server-side, so it can be ruled out first: `artesian-mcp`'s cold handshake
(`initialize` -> `notifications/initialized` -> `tools/list`) takes about 1.3s, the backend opens
lazily, and an unreachable Qdrant produces a typed `-32603` error on the first tool call — the
process itself stays alive and keeps answering `tools/list` correctly. So a stale-resume session
is not the server refusing calls; the client-side tool list is just gone.

A second, unrelated failure class produces a similar symptom: **registration drift**. Several
agent configs on the same machine can each point at a *different* `artesian-mcp` copy — a repo
build, a version-pinned Homebrew Cellar path (`/opt/homebrew/Cellar/artesian/<old-version>/...`),
or a hand-copied binary from weeks ago. Whichever one a given client happens to be registered
against may be missing, broken, or simply out of date.

## One-Command Diagnosis

```sh
artesian doctor --mcp
```

This inventories every registration (project `.mcp.json`, `~/.claude.json` user scope, Codex
`~/.codex/config.toml`, Zed `context_servers`), checks each registered command's path/executable
bit (including following a wrapper script's `exec` target and flagging version-pinned Cellar
paths), compares `--version` across every distinct registered binary against this CLI, drives a
real stdio JSON-RPC handshake (`initialize` / `notifications/initialized` / `tools/list`) against
each distinct registered command, and scans the most recent Claude Code session transcripts for
the stale-resume pattern above. Exits non-zero if anything is fail-level broken.

Sample output (trimmed) on a machine with registration drift and one stale-resumed session:

```text
artesian doctor --mcp (v0.5.9)
  [ok  ] registrations: found 4 artesian MCP registration(s)
           - Claude Code (project .mcp.json): artesian-memory -> artesian-mcp --config /repo/artesian.toml
           - Claude Code (user ~/.claude.json): artesian-memory -> artesian-mcp --config /repo/artesian.toml
           - Codex: artesian-memory -> /opt/homebrew/Cellar/artesian/0.5.2/bin/artesian-mcp --config /repo/artesian.toml
           - Zed: artesian-memory -> artesian-mcp --config /repo/artesian.toml
  [ok  ] path (Claude Code (project .mcp.json): artesian-memory): /opt/homebrew/opt/artesian/bin/artesian-mcp exists and is executable
  [warn] path (Codex: artesian-memory): /opt/homebrew/Cellar/artesian/0.5.2/bin/artesian-mcp is a version-pinned Homebrew Cellar path
           fix: use /opt/homebrew/opt/artesian/bin/artesian-mcp (survives upgrades)
  [warn] version (/opt/homebrew/Cellar/artesian/0.5.2/bin/artesian-mcp): reports 0.5.2, this CLI is 0.5.9
           fix: re-run `artesian init --register-mcp` or update the copy
  [ok  ] handshake (artesian-mcp --config /repo/artesian.toml): responded in 143ms with 6 tool(s)
  [warn] stale-resume: session 1ec52286-8013-4f89-908d-c31cc2d50600 (~/.claude/projects/-repo/1ec52286-....jsonl) removed 'artesian-memory' tools at 2026-06-17T22:01:00.000Z and never re-added them, though its MCP instructions were re-added later — this Claude session sees the server's instructions but cannot call its tools
           fix: start a NEW chat (do not resume this session) — this is an upstream Claude Code resume limitation

3 problem(s) found — see the fixes above
```

`artesian doctor` (no flags) still runs its normal config/backend checks and prints a one-line
hint to also run `--mcp` for the full picture; it does not duplicate the checks above.

## Fix

1. **Stale resumed session**: start a **new chat**. `--resume` cannot recover a session that lost
   its tool registration mid-session — this is Claude Code's limitation, not something
   artesian-mcp or `artesian doctor` can patch from the outside.
2. **Registration drift / stale binary**: re-run `artesian init --register-mcp`. It is
   idempotent: it fixes registrations pointing at a missing or non-executable binary, and if an
   existing registration points at a *different* working binary it leaves it alone and prints a
   drift warning (naming both paths and versions) instead of silently overwriting a deliberate
   override.
3. **Keep one canonical binary path.** Prefer the Homebrew-managed, version-stable path:
   - `brew` installs: `/opt/homebrew/opt/artesian/bin/artesian-mcp` (a symlink that survives
     `brew upgrade`) — never a version-pinned `/opt/homebrew/Cellar/artesian/<version>/...` path,
     and never a hand-copied binary. `artesian init --register-mcp` already prefers this path
     automatically when it detects it is running from a Cellar copy.

## Server-Side Guarantees

These are the properties `artesian doctor --mcp`'s handshake probe and the points above rely on,
and that make the client-side resume issue diagnosable in isolation:

- **Fast, lazy-init handshake.** `initialize` / `tools/list` respond in about 1.3s cold; the
  memory backend is not opened until the first tool call needs it.
- **Typed errors on backend failure.** An unreachable Qdrant (or other backend) surfaces as a
  typed JSON-RPC error (`-32603`) on the call that needed it, not a hang or crash.
- **Survives backend outages.** The server process stays alive and keeps answering `tools/list`
  even while its backend is down.
- **`--log-file` / `ARTESIAN_MCP_LOG`** (flag wins): append plain-text startup (timestamp,
  version, pid, config path), clean stdin-EOF shutdown, and transport/serve error evidence.
  Defaults to `~/.artesian/logs/mcp.log` when unset and `~/.artesian` already exists; rotates once
  to `.1` if the file exceeds 5 MB at startup. Logging is best-effort and never blocks startup.
