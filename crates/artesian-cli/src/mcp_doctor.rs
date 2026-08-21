// SPDX-License-Identifier: Apache-2.0
//! `artesian doctor --mcp`: inventories every artesian MCP registration (Claude Code project +
//! user scope, Codex, Zed), checks the registered command's path/version health, drives a real
//! stdio JSON-RPC handshake against each distinct one, and scans recent Claude Code session
//! transcripts for the "stale resumed session" pattern described in
//! `docs/mcp-troubleshooting.md`: a harness-side MCP restart removes the `mcp__<server>__*` tools
//! mid-session and a `--resume` never gets them back, even though the server's instructions come
//! back on reconnect. That is an upstream Claude Code resume limitation, not an artesian-mcp
//! defect — this module exists to make it diagnosable in one command instead of hours of manual
//! JSONL forensics.
//!
//! The registration-hygiene helpers here (`resolve_registration_command`, `plan_registration`,
//! `existing_command_for`) are also used by `artesian init --register-mcp` (see `main.rs`) so the
//! same Cellar-pinning and drift rules apply whether you're registering or just diagnosing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::process::Command as TokioCommand;

use crate::update::{
    VersionProbe, discover_running_mcp_processes, probe_binary_version, resolve_on_path,
};
use crate::{home_dir, zed_settings_path};

const CELLAR_MARKER: &str = "/Cellar/artesian/";
const CELLAR_STABLE_PATH: &str = "/opt/homebrew/opt/artesian/bin/artesian-mcp";
const MAX_STALE_RESUME_SESSIONS: usize = 6;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Dedup key for the handshake probe: (command, args, sorted env) — two registrations that agree
/// on all three would produce an identical handshake, so only probe one of them.
type HandshakeDedupKey = (String, Vec<String>, Vec<(String, String)>);

// ---------------------------------------------------------------------------------------------
// Registration sources: where an artesian MCP server can be registered.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistrationKind {
    ClaudeProject,
    ClaudeUser,
    Codex,
    Zed,
}

#[derive(Debug, Clone)]
pub(crate) struct RegistrationSource {
    pub label: &'static str,
    pub path: PathBuf,
    pub kind: RegistrationKind,
}

/// The four places `artesian init --register-mcp` writes to, resolved for this machine (some
/// may not exist yet — that is not itself a problem, just an empty inventory contribution).
pub(crate) fn registration_sources() -> Vec<RegistrationSource> {
    let mut sources = vec![RegistrationSource {
        label: "Claude Code (project .mcp.json)",
        path: PathBuf::from(".mcp.json"),
        kind: RegistrationKind::ClaudeProject,
    }];
    if let Ok(home) = home_dir() {
        sources.push(RegistrationSource {
            label: "Claude Code (user ~/.claude.json)",
            path: home.join(".claude.json"),
            kind: RegistrationKind::ClaudeUser,
        });
        sources.push(RegistrationSource {
            label: "Codex",
            path: home.join(".codex").join("config.toml"),
            kind: RegistrationKind::Codex,
        });
    }
    if let Ok(path) = zed_settings_path() {
        sources.push(RegistrationSource {
            label: "Zed",
            path,
            kind: RegistrationKind::Zed,
        });
    }
    sources
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawServerEntry {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Every `mcpServers`/`mcp_servers`/`context_servers` entry declared at `source.path`, keyed by
/// server name. Best-effort: a missing or unparsable file yields an empty map, never an error —
/// doctor should never crash because some *other* client's config is malformed.
fn read_entries(source: &RegistrationSource) -> BTreeMap<String, RawServerEntry> {
    match source.kind {
        RegistrationKind::ClaudeProject | RegistrationKind::ClaudeUser => {
            read_claude_style_json(&source.path)
        }
        RegistrationKind::Codex => read_codex_toml(&source.path),
        RegistrationKind::Zed => read_zed_json(&source.path),
    }
}

fn read_claude_style_json(path: &Path) -> BTreeMap<String, RawServerEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    servers
        .iter()
        .map(|(name, entry)| (name.clone(), raw_entry_from_json(entry)))
        .collect()
}

fn read_zed_json(path: &Path) -> BTreeMap<String, RawServerEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    let Some(servers) = root.get("context_servers").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    servers
        .iter()
        .map(|(name, entry)| {
            let command = entry.get("command").cloned().unwrap_or(Value::Null);
            (name.clone(), raw_entry_from_json(&command))
        })
        .collect()
}

fn raw_entry_from_json(entry: &Value) -> RawServerEntry {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| entry.get("path").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let env = entry
        .get("env")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    RawServerEntry { command, args, env }
}

fn read_codex_toml(path: &Path) -> BTreeMap<String, RawServerEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return BTreeMap::new();
    };
    let Some(servers) = document.get("mcp_servers").and_then(|item| item.as_table()) else {
        return BTreeMap::new();
    };
    servers
        .iter()
        .map(|(name, item)| {
            let table = item.as_table_like();
            let command = table
                .and_then(|t| t.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let args = table
                .and_then(|t| t.get("args"))
                .and_then(|v| v.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let env = table
                .and_then(|t| t.get("env"))
                .and_then(|v| v.as_table_like())
                .map(|env_table| {
                    env_table
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            (name.to_string(), RawServerEntry { command, args, env })
        })
        .collect()
}

/// The existing command registered for `server_name` at `source`, if any (used by `init
/// --register-mcp` to decide whether it is safe to overwrite).
pub(crate) fn existing_command_for(
    source: &RegistrationSource,
    server_name: &str,
) -> Option<String> {
    read_entries(source)
        .get(server_name)
        .map(|entry| entry.command.clone())
        .filter(|command| !command.is_empty())
}

// ---------------------------------------------------------------------------------------------
// Path / executable / Cellar-pinning helpers, shared by the doctor checks and by init's hygiene.
// ---------------------------------------------------------------------------------------------

/// Resolve `command` to a concrete path: absolute/relative paths are checked directly, bare
/// names are looked up on `PATH` (same rule the OS uses when a client execs the registered
/// command).
fn resolve_registered_command(command: &str) -> Option<PathBuf> {
    resolve_on_path(command)
}

pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// If `path` is a shebang script, return the token following its last `exec` line (the binary a
/// wrapper like `artesian init`'s Qdrant-key shim ultimately hands off to).
pub(crate) fn script_exec_target(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    if !content.starts_with("#!") {
        return None;
    }
    content.lines().rev().find_map(|line| {
        let rest = line.trim().strip_prefix("exec ")?;
        rest.split_whitespace()
            .next()
            .map(|token| token.trim_matches('"').to_string())
    })
}

pub(crate) fn is_cellar_pinned(path: &str) -> bool {
    path.contains(CELLAR_MARKER)
}

/// Decide what command `init --register-mcp` should write. If the *currently running* `artesian`
/// binary is itself a version-pinned Homebrew Cellar copy, prefer the version-stable `opt`
/// symlink so the registration survives the next `brew upgrade` — but only if that symlink
/// actually exists; otherwise keep whatever would have been registered anyway.
pub(crate) fn resolve_registration_command(default_command: &str) -> String {
    match std::env::current_exe() {
        Ok(exe)
            if is_cellar_pinned(&exe.display().to_string())
                && Path::new(CELLAR_STABLE_PATH).is_file() =>
        {
            CELLAR_STABLE_PATH.to_string()
        }
        _ => default_command.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegistrationAction {
    /// Safe to (re)write. `replaced_stale` is true when an existing entry was overwritten
    /// because it pointed at a missing/non-executable path.
    Write { replaced_stale: bool },
    /// An existing, working registration points at a *different* binary than the one we'd
    /// write — leave it alone rather than silently clobbering a deliberate override.
    Skip { existing_command: String },
}

pub(crate) fn plan_registration(existing: Option<&str>, new_command: &str) -> RegistrationAction {
    match existing {
        None => RegistrationAction::Write {
            replaced_stale: false,
        },
        Some(existing) if existing == new_command => RegistrationAction::Write {
            replaced_stale: false,
        },
        Some(existing) => {
            let still_works =
                resolve_registered_command(existing).is_some_and(|path| is_executable(&path));
            if still_works {
                RegistrationAction::Skip {
                    existing_command: existing.to_string(),
                }
            } else {
                RegistrationAction::Write {
                    replaced_stale: true,
                }
            }
        }
    }
}

fn version_token(full: &str) -> &str {
    full.rsplit(' ').next().unwrap_or(full).trim()
}

/// A short human label for `command`'s reported version, for drift warnings. Best-effort: any
/// failure to resolve/run/parse becomes a short descriptive string, never an error.
pub(crate) fn probe_version_label(command: &str) -> String {
    match resolve_registered_command(command).map(|path| probe_binary_version(&path)) {
        Some(VersionProbe::Found(text)) => version_token(&text).to_string(),
        Some(VersionProbe::Failed(message)) => format!("error: {message}"),
        Some(VersionProbe::NotInstalled) | None => "not found".to_string(),
    }
}

/// The binary a registration ultimately runs: follows one level of shebang-script `exec`
/// indirection (e.g. the Qdrant-key wrapper) and resolves bare names on `PATH`. Falls back to
/// the raw command string when nothing resolves, so callers always have a display value.
fn final_target(command: &str) -> String {
    let Some(resolved) = resolve_registered_command(command) else {
        return command.to_string();
    };
    match script_exec_target(&resolved) {
        Some(target) => resolve_registered_command(&target)
            .map(|path| path.display().to_string())
            .unwrap_or(target),
        None => resolved.display().to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Inventory: every artesian-looking registration across all four sources.
// ---------------------------------------------------------------------------------------------

pub(crate) struct McpRegistrationEntry {
    pub source_label: &'static str,
    pub server_name: String,
    pub entry: RawServerEntry,
}

fn is_artesian_like(name: &str, entry: &RawServerEntry) -> bool {
    if name.to_ascii_lowercase().contains("artesian") {
        return true;
    }
    let command_basename = Path::new(&entry.command)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    if command_basename.contains("artesian-mcp") {
        return true;
    }
    resolve_registered_command(&entry.command)
        .and_then(|path| script_exec_target(&path))
        .is_some_and(|target| target.contains("artesian-mcp"))
}

fn collect_registrations() -> Vec<McpRegistrationEntry> {
    let mut found = Vec::new();
    for source in registration_sources() {
        if !source.path.exists() {
            continue;
        }
        for (name, entry) in read_entries(&source) {
            if is_artesian_like(&name, &entry) {
                found.push(McpRegistrationEntry {
                    source_label: source.label,
                    server_name: name,
                    entry,
                });
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------------------------
// Live stdio JSON-RPC handshake probe (initialize -> notifications/initialized -> tools/list).
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandshakeOk {
    pub elapsed_ms: u128,
    pub tools_count: usize,
}

pub(crate) async fn probe_handshake(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<HandshakeOk, String> {
    probe_handshake_with_timeout(command, args, env, HANDSHAKE_TIMEOUT).await
}

pub(crate) async fn probe_handshake_with_timeout(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    budget: Duration,
) -> Result<HandshakeOk, String> {
    match tokio::time::timeout(budget, probe_handshake_inner(command, args, env)).await {
        Ok(Ok(ok)) => Ok(ok),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("handshake timed out".to_string()),
    }
}

async fn probe_handshake_inner(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<HandshakeOk> {
    let start = Instant::now();
    let mut child = TokioCommand::new(command)
        .args(args)
        .envs(env.iter())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn `{command}`"))?;
    let mut stdin = child.stdin.take().context("child has no stdin")?;
    let stdout = child.stdout.take().context("child has no stdout")?;
    let mut reader = tokio::io::BufReader::new(stdout);

    let result: Result<HandshakeOk> = async {
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "artesian-doctor", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        )
        .await?;
        let _initialize_response = read_json_line(&mut reader).await?;

        write_json_line(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await?;

        write_json_line(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .await?;
        let tools_response = read_json_line(&mut reader).await?;

        let tools_count = tools_response
            .get("result")
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        Ok(HandshakeOk {
            elapsed_ms: start.elapsed().as_millis(),
            tools_count,
        })
    }
    .await;

    let _ = child.start_kill();
    let _ = child.wait().await;
    result
}

async fn write_json_line(stdin: &mut tokio::process::ChildStdin, value: &Value) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_json_line(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
) -> Result<Value> {
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    let bytes = reader.read_line(&mut line).await?;
    if bytes == 0 {
        anyhow::bail!("child closed its stdout before responding");
    }
    serde_json::from_str(line.trim())
        .with_context(|| format!("parse JSON-RPC response line: {}", line.trim()))
}

// ---------------------------------------------------------------------------------------------
// Stale-resume detection: the "wife's 2-hour forensics" as one pass over recent session JSONL.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleResumeFinding {
    pub session_file: PathBuf,
    pub session_id: Option<String>,
    pub server_name: String,
    pub removed_at: Option<String>,
}

/// Claude Code's session-transcript directory encoding: the absolute cwd with `/` and `.`
/// replaced by `-` (verified against the real `~/.claude/projects` layout on this machine).
fn encode_claude_project_dir(cwd: &Path) -> String {
    cwd.display()
        .to_string()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn claude_projects_dir_for(home: &Path, cwd: &Path) -> PathBuf {
    home.join(".claude")
        .join("projects")
        .join(encode_claude_project_dir(cwd))
}

/// The `max` most-recently-modified `*.jsonl` files directly under `dir`, newest first.
fn recent_session_files(dir: &Path, max: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.into_iter().take(max).map(|(_, path)| path).collect()
}

/// `mcp__<server>__<tool>` -> `(server, tool)`. Server names use hyphens/single underscores, so
/// the first double-underscore after the `mcp__` prefix is always the separator.
fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let idx = rest.find("__")?;
    Some((&rest[..idx], &rest[idx + 2..]))
}

fn server_is_artesian(server: &str) -> bool {
    server.to_ascii_lowercase().contains("artesian")
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct ServerDeltaState {
    removed_at: Option<String>,
    readded_after_removal: bool,
    instructions_readded_at: Option<String>,
}

/// Stream one session transcript line-by-line (never materializing the whole file — these can be
/// huge), looking only at `deferred_tools_delta`/`mcp_instructions_delta` attachment lines, and
/// flag any artesian server that was removed and never re-added, while its instructions came back
/// later — the signature of a stale resumed session.
fn analyze_session_file(path: &Path) -> Result<Vec<StaleResumeFinding>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut per_server: BTreeMap<String, ServerDeltaState> = BTreeMap::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if !(line.contains("deferred_tools_delta") || line.contains("mcp_instructions_delta")) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if session_id.is_none() {
            session_id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(attachment) = value.get("attachment") else {
            continue;
        };

        match attachment.get("type").and_then(Value::as_str) {
            Some("deferred_tools_delta") => {
                let removed = string_list(attachment.get("removedNames"));
                let added = string_list(attachment.get("addedNames"));
                let readded = string_list(attachment.get("readdedNames"));
                for name in &removed {
                    if let Some((server, _tool)) = parse_mcp_tool_name(name) {
                        if server_is_artesian(server) {
                            let state = per_server.entry(server.to_string()).or_default();
                            state.removed_at = timestamp.clone();
                            state.readded_after_removal = false;
                            state.instructions_readded_at = None;
                        }
                    }
                }
                for name in added.iter().chain(readded.iter()) {
                    if let Some((server, _tool)) = parse_mcp_tool_name(name) {
                        if let Some(state) = per_server.get_mut(server) {
                            if state.removed_at.is_some() {
                                state.readded_after_removal = true;
                            }
                        }
                    }
                }
            }
            Some("mcp_instructions_delta") => {
                for server in string_list(attachment.get("addedNames")) {
                    if server_is_artesian(&server) {
                        if let Some(state) = per_server.get_mut(&server) {
                            if state.removed_at.is_some() && !state.readded_after_removal {
                                state.instructions_readded_at = timestamp.clone();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(per_server
        .into_iter()
        .filter(|(_, state)| {
            state.removed_at.is_some()
                && !state.readded_after_removal
                && state.instructions_readded_at.is_some()
        })
        .map(|(server_name, state)| StaleResumeFinding {
            session_file: path.to_path_buf(),
            session_id: session_id.clone(),
            server_name,
            removed_at: state.removed_at,
        })
        .collect())
}

fn stale_resume_findings(cwd: &Path) -> Result<Vec<StaleResumeFinding>> {
    let home = home_dir()?;
    let dir = claude_projects_dir_for(&home, cwd);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut findings = Vec::new();
    for path in recent_session_files(&dir, MAX_STALE_RESUME_SESSIONS) {
        findings.extend(analyze_session_file(&path)?);
    }
    Ok(findings)
}

// ---------------------------------------------------------------------------------------------
// Top-level `artesian doctor --mcp` report.
// ---------------------------------------------------------------------------------------------

enum Level {
    Ok,
    Warn,
    Fail,
}

fn report(level: Level, line: &str, fix: Option<&str>, problems: &mut usize) {
    let tag = match level {
        Level::Ok => "ok",
        Level::Warn => "warn",
        Level::Fail => "fail",
    };
    println!("  [{tag:<4}] {line}");
    if let Some(fix) = fix {
        println!("           fix: {fix}");
    }
    if matches!(level, Level::Fail) {
        *problems += 1;
    }
}

/// Physical footprint of a process, in MB.
///
/// Deliberately not RSS: a server that has been swapped out reports a few hundred kilobytes while
/// still owing hundreds of megabytes, and a machine deep enough in swap to do that is exactly the
/// one this check exists to explain.
#[cfg(target_os = "macos")]
fn process_footprint_mb(pid: u32) -> Option<f64> {
    let output = std::process::Command::new("vmmap")
        .args(["-summary", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let raw = text
        .lines()
        .find(|line| line.starts_with("Physical footprint:"))?
        .split(':')
        .nth(1)?
        .trim()
        .to_string();
    let (value, scale) = match raw.chars().last()? {
        'G' => (&raw[..raw.len() - 1], 1024.0),
        'M' => (&raw[..raw.len() - 1], 1.0),
        'K' => (&raw[..raw.len() - 1], 1.0 / 1024.0),
        _ => (raw.as_str(), 1.0 / (1024.0 * 1024.0)),
    };
    Some(value.trim().parse::<f64>().ok()? * scale)
}

#[cfg(not(target_os = "macos"))]
fn process_footprint_mb(_pid: u32) -> Option<f64> {
    None
}

/// Combined footprint past which the running servers are worth a second look.
///
/// Not a correctness threshold — one server per open client is normal. It is the point where the
/// total stops being background noise on a laptop.
const MCP_FOOTPRINT_WARN_MB: f64 = 4096.0;

/// Inventory the live `artesian-mcp` processes and what they cost together.
///
/// The useful number is the total, not the count: every server holds its own copy of the embedding
/// model, so an abandoned client session keeps paying for its servers until something reclaims
/// them. Seeing the sum is what turns "a lot of processes" into a decision.
fn check_running_processes(problems: &mut usize) {
    let processes = match discover_running_mcp_processes() {
        Ok(processes) => processes,
        Err(error) => {
            report(
                Level::Warn,
                &format!("processes: could not inventory running artesian-mcp processes: {error}"),
                None,
                problems,
            );
            return;
        }
    };

    if processes.is_empty() {
        report(
            Level::Ok,
            "processes: no artesian-mcp servers are running",
            None,
            problems,
        );
        return;
    }

    let measured = processes
        .iter()
        .map(|process| (process, process_footprint_mb(process.pid)))
        .collect::<Vec<_>>();
    let total_mb = measured
        .iter()
        .filter_map(|(_, footprint)| *footprint)
        .sum::<f64>();
    let unmeasured = measured
        .iter()
        .filter(|(_, footprint)| footprint.is_none())
        .count();

    let summary = if total_mb > 0.0 {
        format!(
            "processes: {} artesian-mcp server(s) running, {:.1} GB combined",
            processes.len(),
            total_mb / 1024.0
        )
    } else {
        format!(
            "processes: {} artesian-mcp server(s) running",
            processes.len()
        )
    };

    let level = if total_mb >= MCP_FOOTPRINT_WARN_MB {
        Level::Warn
    } else {
        Level::Ok
    };
    let fix = (total_mb >= MCP_FOOTPRINT_WARN_MB).then_some(
        "each server holds its own embedding model — close idle MCP clients, or run `artesian update --restart-stale` to drop servers left by clients that already exited",
    );
    report(level, &summary, fix, problems);

    for (process, footprint) in &measured {
        let cost = footprint
            .map(|mb| format!("{mb:>8.1} MB"))
            .unwrap_or_else(|| "        ? MB".to_string());
        println!(
            "           - pid {:<7} {cost}  {}",
            process.pid, process.command
        );
    }
    if unmeasured > 0 {
        println!("           ({unmeasured} process(es) reported no footprint)");
    }
}

fn check_path_health(reg: &McpRegistrationEntry, problems: &mut usize) {
    let label = format!("path ({}: {})", reg.source_label, reg.server_name);
    let Some(resolved) = resolve_registered_command(&reg.entry.command) else {
        report(
            Level::Fail,
            &format!(
                "{label}: command '{}' was not found (not on PATH, not a file)",
                reg.entry.command
            ),
            Some(
                "re-run `artesian init --register-mcp`, or repoint the registration at an installed artesian-mcp",
            ),
            problems,
        );
        return;
    };
    if !is_executable(&resolved) {
        report(
            Level::Fail,
            &format!(
                "{label}: {} exists but is not executable",
                resolved.display()
            ),
            Some(&format!("chmod +x {}", resolved.display())),
            problems,
        );
        return;
    }
    if is_cellar_pinned(&resolved.display().to_string()) {
        report(
            Level::Warn,
            &format!(
                "{label}: {} is a version-pinned Homebrew Cellar path",
                resolved.display()
            ),
            Some(&format!("use {CELLAR_STABLE_PATH} (survives upgrades)")),
            problems,
        );
    } else {
        report(
            Level::Ok,
            &format!("{label}: {} exists and is executable", resolved.display()),
            None,
            problems,
        );
    }

    let Some(target) = script_exec_target(&resolved) else {
        return;
    };
    let target_label = format!("{label} script exec target '{target}'");
    match resolve_registered_command(&target) {
        Some(target_path) if is_executable(&target_path) => {
            if is_cellar_pinned(&target_path.display().to_string()) {
                report(
                    Level::Warn,
                    &format!(
                        "{target_label}: {} is a version-pinned Homebrew Cellar path",
                        target_path.display()
                    ),
                    Some(&format!("use {CELLAR_STABLE_PATH} (survives upgrades)")),
                    problems,
                );
            } else {
                report(
                    Level::Ok,
                    &format!(
                        "{target_label}: {} exists and is executable",
                        target_path.display()
                    ),
                    None,
                    problems,
                );
            }
        }
        Some(target_path) => report(
            Level::Fail,
            &format!(
                "{target_label}: {} exists but is not executable",
                target_path.display()
            ),
            Some(&format!("chmod +x {}", target_path.display())),
            problems,
        ),
        None => report(
            Level::Fail,
            &format!("{target_label}: not found on PATH"),
            Some("install artesian-mcp on PATH (e.g. `brew install aquifer-labs/tap/artesian`)"),
            problems,
        ),
    }
}

fn check_version_drift(target: &str, current_version: &str, problems: &mut usize) {
    let label = format!("version ({target})");
    let Some(path) = resolve_registered_command(target) else {
        report(
            Level::Warn,
            &format!("{label}: could not resolve to run --version"),
            Some("verify the binary is installed and on PATH"),
            problems,
        );
        return;
    };
    match probe_binary_version(&path) {
        VersionProbe::Found(text) => {
            let found = version_token(&text);
            if found == current_version {
                report(
                    Level::Ok,
                    &format!("{label}: {found} matches this CLI ({current_version})"),
                    None,
                    problems,
                );
            } else {
                report(
                    Level::Warn,
                    &format!("{label}: reports {found}, this CLI is {current_version}"),
                    Some("re-run `artesian init --register-mcp` or update the copy"),
                    problems,
                );
            }
        }
        VersionProbe::Failed(message) => report(
            Level::Warn,
            &format!("{label}: --version failed ({message})"),
            Some("re-run `artesian init --register-mcp` or update the copy"),
            problems,
        ),
        VersionProbe::NotInstalled => report(
            Level::Warn,
            &format!("{label}: not installed"),
            Some("re-run `artesian init --register-mcp` or update the copy"),
            problems,
        ),
    }
}

async fn check_handshake(entry: &RawServerEntry, problems: &mut usize) {
    let label = format!("handshake ({} {})", entry.command, entry.args.join(" "));
    match probe_handshake(&entry.command, &entry.args, &entry.env).await {
        Ok(ok) => report(
            Level::Ok,
            &format!(
                "{label}: responded in {}ms with {} tool(s)",
                ok.elapsed_ms, ok.tools_count
            ),
            None,
            problems,
        ),
        Err(error) => report(
            Level::Fail,
            &format!("{label}: {error}"),
            Some(
                "run the command manually and check stderr, or re-run `artesian init --register-mcp`",
            ),
            problems,
        ),
    }
}

fn check_stale_resume(cwd: &Path, problems: &mut usize) {
    match stale_resume_findings(cwd) {
        Ok(findings) if findings.is_empty() => report(
            Level::Ok,
            "stale-resume: no stale resumed sessions detected in the most recent transcripts",
            None,
            problems,
        ),
        Ok(findings) => {
            for finding in findings {
                let removed_at = finding.removed_at.as_deref().unwrap_or("unknown time");
                report(
                    Level::Warn,
                    &format!(
                        "stale-resume: session {} ({}) removed '{}' tools at {removed_at} and \
                         never re-added them, though its MCP instructions were re-added later — \
                         this Claude session sees the server's instructions but cannot call its tools",
                        finding.session_id.as_deref().unwrap_or("unknown"),
                        finding.session_file.display(),
                        finding.server_name,
                    ),
                    Some(
                        "start a NEW chat (do not resume this session) — this is an upstream Claude Code resume limitation",
                    ),
                    problems,
                );
            }
        }
        Err(error) => report(
            Level::Warn,
            &format!("stale-resume: could not scan Claude Code session transcripts: {error}"),
            Some("check read permissions on ~/.claude/projects"),
            problems,
        ),
    }
}

/// Run all `--mcp` checks and print an ok/warn/fail report. Returns an error (after printing
/// everything) when any fail-level finding was seen, so scripts can gate on the exit code.
pub(crate) async fn run(cwd: &Path) -> Result<()> {
    println!("artesian doctor --mcp (v{})", env!("CARGO_PKG_VERSION"));
    let mut problems = 0usize;

    let registrations = collect_registrations();
    if registrations.is_empty() {
        report(
            Level::Warn,
            "registrations: no artesian MCP registrations found",
            Some("run `artesian init --register-mcp`"),
            &mut problems,
        );
    } else {
        report(
            Level::Ok,
            &format!(
                "registrations: found {} artesian MCP registration(s)",
                registrations.len()
            ),
            None,
            &mut problems,
        );
        for reg in &registrations {
            println!(
                "           - {}: {} -> {} {}",
                reg.source_label,
                reg.server_name,
                reg.entry.command,
                reg.entry.args.join(" ")
            );
        }
    }

    for reg in &registrations {
        check_path_health(reg, &mut problems);
    }

    check_running_processes(&mut problems);

    let current_version = env!("CARGO_PKG_VERSION");
    let mut seen_targets: BTreeSet<String> = BTreeSet::new();
    for reg in &registrations {
        // Probe the binary a registration ultimately runs, not the wrapper script: wrappers
        // hardcode their args (no `"$@"`), so `--version` against the wrapper starts a real
        // server that then fails the probe.
        let target = final_target(&reg.entry.command);
        if seen_targets.insert(target.clone()) {
            check_version_drift(&target, current_version, &mut problems);
        }
    }

    let mut seen_commands: BTreeSet<HandshakeDedupKey> = BTreeSet::new();
    for reg in &registrations {
        let key = (
            reg.entry.command.clone(),
            reg.entry.args.clone(),
            reg.entry.env.clone().into_iter().collect(),
        );
        if seen_commands.insert(key) {
            check_handshake(&reg.entry, &mut problems).await;
        }
    }

    check_stale_resume(cwd, &mut problems);

    println!();
    if problems == 0 {
        println!("\u{2713} all --mcp checks passed");
        Ok(())
    } else {
        anyhow::bail!("{problems} problem(s) found — see the fixes above")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn cellar_path_is_detected() {
        assert!(is_cellar_pinned(
            "/opt/homebrew/Cellar/artesian/0.5.2/bin/artesian-mcp"
        ));
        assert!(!is_cellar_pinned(
            "/opt/homebrew/opt/artesian/bin/artesian-mcp"
        ));
        assert!(!is_cellar_pinned("artesian-mcp"));
    }

    #[test]
    fn resolve_registration_command_keeps_default_when_not_cellar_pinned() {
        // current_exe() in the test harness is never a Cellar path, so this should be a no-op.
        assert_eq!(resolve_registration_command("artesian-mcp"), "artesian-mcp");
    }

    #[test]
    fn script_exec_target_extracts_final_exec_binary() {
        let dir = tempdir();
        let path = dir.join("run-artesian-mcp.sh");
        fs::write(
            &path,
            "#!/bin/sh\nif [ -z \"$QDRANT_API_KEY\" ]; then\n  true\nfi\nexec artesian-mcp \"$@\"\n",
        )
        .unwrap();
        assert_eq!(script_exec_target(&path), Some("artesian-mcp".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_exec_target_none_for_non_script() {
        let dir = tempdir();
        let path = dir.join("not-a-script");
        fs::write(&path, "just some text\n").unwrap();
        assert_eq!(script_exec_target(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_registration_writes_when_nothing_registered_yet() {
        assert_eq!(
            plan_registration(None, "artesian-mcp"),
            RegistrationAction::Write {
                replaced_stale: false
            }
        );
    }

    #[test]
    fn plan_registration_writes_when_identical() {
        assert_eq!(
            plan_registration(Some("artesian-mcp"), "artesian-mcp"),
            RegistrationAction::Write {
                replaced_stale: false
            }
        );
    }

    #[test]
    fn plan_registration_replaces_missing_binary() {
        let action = plan_registration(
            Some("/definitely/does/not/exist/artesian-mcp"),
            "artesian-mcp",
        );
        assert_eq!(
            action,
            RegistrationAction::Write {
                replaced_stale: true
            }
        );
    }

    #[test]
    fn plan_registration_skips_when_existing_binary_still_works() {
        let dir = tempdir();
        let existing = dir.join("artesian-mcp-old");
        fs::write(&existing, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let action = plan_registration(Some(existing.to_str().unwrap()), "artesian-mcp");
        assert_eq!(
            action,
            RegistrationAction::Skip {
                existing_command: existing.to_str().unwrap().to_string()
            }
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_token_takes_last_whitespace_separated_word() {
        assert_eq!(version_token("artesian-mcp 0.5.9"), "0.5.9");
        assert_eq!(version_token("0.5.9"), "0.5.9");
    }

    #[test]
    fn encode_claude_project_dir_replaces_slash_and_dot() {
        assert_eq!(
            encode_claude_project_dir(Path::new("/Users/alice/Documents/git/artesian")),
            "-Users-alice-Documents-git-artesian"
        );
        assert_eq!(
            encode_claude_project_dir(Path::new("/Users/alice/proj.name")),
            "-Users-alice-proj-name"
        );
    }

    #[test]
    fn parse_mcp_tool_name_splits_on_first_double_underscore() {
        assert_eq!(
            parse_mcp_tool_name("mcp__artesian-memory__memory_find"),
            Some(("artesian-memory", "memory_find"))
        );
        assert_eq!(
            parse_mcp_tool_name("mcp__claude_ai_Gmail__apply_sensitive_message_label"),
            Some(("claude_ai_Gmail", "apply_sensitive_message_label"))
        );
        assert_eq!(parse_mcp_tool_name("EnterWorktree"), None);
    }

    fn tempdir() -> PathBuf {
        // pid + timestamp alone can collide when parallel tests hit the same clock tick —
        // a per-process counter makes every call unique.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artesian-mcp-doctor-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn jsonl_line(json: Value) -> String {
        serde_json::to_string(&json).unwrap()
    }

    /// removal -> no re-add -> instructions re-added later = flagged.
    #[test]
    fn analyze_session_flags_removal_never_readded_with_instructions_back() {
        let dir = tempdir();
        let path = dir.join("session.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-1",
                "timestamp": "2026-06-17T22:00:00.000Z",
                "attachment": {
                    "type": "deferred_tools_delta",
                    "addedNames": ["mcp__artesian-memory__memory_find"],
                    "removedNames": [],
                    "readdedNames": []
                }
            }))
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-1",
                "timestamp": "2026-06-17T22:01:00.000Z",
                "attachment": {
                    "type": "deferred_tools_delta",
                    "addedNames": [],
                    "removedNames": ["mcp__artesian-memory__memory_find"],
                    "readdedNames": []
                }
            }))
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-1",
                "timestamp": "2026-06-17T22:03:45.518Z",
                "attachment": {
                    "type": "mcp_instructions_delta",
                    "addedNames": ["artesian-memory"],
                    "removedNames": []
                }
            }))
        )
        .unwrap();
        drop(file);

        let findings = analyze_session_file(&path).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].server_name, "artesian-memory");
        assert_eq!(findings[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            findings[0].removed_at.as_deref(),
            Some("2026-06-17T22:01:00.000Z")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// removal -> re-add = clean (no finding), even if instructions come back too.
    #[test]
    fn analyze_session_is_clean_when_tools_are_readded() {
        let dir = tempdir();
        let path = dir.join("session.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-2",
                "timestamp": "2026-06-17T22:01:00.000Z",
                "attachment": {
                    "type": "deferred_tools_delta",
                    "addedNames": [],
                    "removedNames": ["mcp__artesian-memory__memory_find"],
                    "readdedNames": []
                }
            }))
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-2",
                "timestamp": "2026-06-17T22:02:00.000Z",
                "attachment": {
                    "type": "deferred_tools_delta",
                    "addedNames": [],
                    "removedNames": [],
                    "readdedNames": ["mcp__artesian-memory__memory_find"]
                }
            }))
        )
        .unwrap();
        writeln!(
            file,
            "{}",
            jsonl_line(json!({
                "sessionId": "sess-2",
                "timestamp": "2026-06-17T22:03:45.518Z",
                "attachment": {
                    "type": "mcp_instructions_delta",
                    "addedNames": ["artesian-memory"],
                    "removedNames": []
                }
            }))
        )
        .unwrap();
        drop(file);

        let findings = analyze_session_file(&path).unwrap();
        assert!(findings.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probe_handshake_reports_tools_from_fake_server() {
        let dir = tempdir();
        let script = dir.join("fake-mcp.sh");
        fs::write(
            &script,
            "#!/bin/sh\n\
             read -r _init\n\
             printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"capabilities\":{}}}\\n'\n\
             read -r _initialized\n\
             read -r _list\n\
             printf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"memory_find\"},{\"name\":\"memory_store\"}]}}\\n'\n\
             sleep 2\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = probe_handshake_with_timeout(
            script.to_str().unwrap(),
            &[],
            &BTreeMap::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("handshake should succeed against the fake server");
        assert_eq!(result.tools_count, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probe_handshake_times_out_against_a_silent_process() {
        let dir = tempdir();
        let script = dir.join("silent.sh");
        fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let result = probe_handshake_with_timeout(
            script.to_str().unwrap(),
            &[],
            &BTreeMap::new(),
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(result, Err("handshake timed out".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_claude_style_json_parses_mcp_servers() {
        let dir = tempdir();
        let path = dir.join(".mcp.json");
        fs::write(
            &path,
            r#"{"mcpServers":{"artesian-memory":{"command":"artesian-mcp","args":["--config","artesian.toml"],"env":{"FOO":"bar"}}}}"#,
        )
        .unwrap();
        let entries = read_claude_style_json(&path);
        let entry = entries.get("artesian-memory").expect("entry present");
        assert_eq!(entry.command, "artesian-mcp");
        assert_eq!(entry.args, vec!["--config", "artesian.toml"]);
        assert_eq!(entry.env.get("FOO"), Some(&"bar".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_codex_toml_parses_mcp_servers_table() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        fs::write(
            &path,
            "[mcp_servers.artesian-memory]\ncommand = \"artesian-mcp\"\nargs = [\"--config\", \"artesian.toml\"]\n\n[mcp_servers.artesian-memory.env]\nARTESIAN_MCP_TOOL_HINT = \"hint\"\n",
        )
        .unwrap();
        let entries = read_codex_toml(&path);
        let entry = entries.get("artesian-memory").expect("entry present");
        assert_eq!(entry.command, "artesian-mcp");
        assert_eq!(entry.args, vec!["--config", "artesian.toml"]);
        assert_eq!(
            entry.env.get("ARTESIAN_MCP_TOOL_HINT"),
            Some(&"hint".to_string())
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
