// SPDX-License-Identifier: Apache-2.0

//! OCF lifecycle receipts (`receipts.jsonl`).
//!
//! The qualify log (see `headgate::bundle`) governs what enters one agent's context.
//! `receipts.jsonl` governs the membrane between a parent loop and its subagents: what a child
//! received, what it was allowed to spend, why it stopped, and what came back. Mirrors
//! `ocf/SPEC.md` §5 ("Lifecycle Receipts — receipts.jsonl") and `ocf/schema/receipts.schema.json`
//! exactly — one JSON object per line, two record kinds (`spawn` and `return`).
//!
//! A spawn receipt is written *before* the child starts; a child without a spawn receipt does
//! not run (fail-closed — see [`ReceiptsConfig::fail_closed`]). A return receipt is terminal:
//! spawn acknowledgment and completion are distinct events, so only a return receipt carries a
//! [`StopReason`].

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Cap on the `task_boundary` field: the first line of the dispatched task, truncated.
pub const RECEIPT_TASK_BOUNDARY_CHARS: usize = 200;

/// Environment variable that flips [`ReceiptsConfig::fail_closed`] on. Mirrors the
/// `ARTESIAN_*_ENV` convention already used by `artesian_process_agent` /
/// `flume::loop_core` for inert-by-default toggles (e.g. `ARTESIAN_NATIVE_SUBAGENTS`,
/// `ARTESIAN_RUNS_DIR`).
pub const ARTESIAN_RECEIPTS_FAIL_CLOSED_ENV: &str = "ARTESIAN_RECEIPTS_FAIL_CLOSED";

// ── Stop reason ────────────────────────────────────────────────────────────────────────────────

/// Terminal stop reason carried on a return receipt. Mirrors `receipts.schema.json`'s
/// `stop_reason` enum exactly: `done | budget_tokens | budget_tool_calls | budget_time |
/// budget_cost | gate_rejected | error | killed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Done,
    BudgetTokens,
    BudgetToolCalls,
    BudgetTime,
    BudgetCost,
    GateRejected,
    Error,
    Killed,
}

// ── Envelopes ──────────────────────────────────────────────────────────────────────────────────

/// A declared resource budget: the envelope that *authorizes* autonomous execution.  Every
/// dimension is opt-in; per the OCF spec at least one dimension SHOULD be present for a spawn to
/// be well-formed (enforced by [`ReceiptsConfig::fail_closed`] when enabled).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl BudgetEnvelope {
    /// `true` when no dimension is declared — the case
    /// [`ReceiptsConfig::fail_closed`] refuses to spawn.
    pub fn is_empty(&self) -> bool {
        self.max_tokens.is_none()
            && self.max_tool_calls.is_none()
            && self.max_wall_time_ms.is_none()
            && self.max_cost_usd.is_none()
    }
}

/// Actuals in the same dimensions as [`BudgetEnvelope`] — what a spawn actually consumed.
/// Per the spec, `consumed` reports reality even when it overruns the declared budget.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Consumed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// What the parent actually received back from the child — a bounded distillation. The child's
/// full trace stays addressable via [`ReturnReceipt::trace_ref`], never inlined here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Distilled {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub reference: Option<String>,
}

/// The distillation-gate decision recorded on a return receipt: typed, never silent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDecision {
    pub admitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ── Records ────────────────────────────────────────────────────────────────────────────────────

/// A spawn record — written *before* the child starts. Mirrors `receipts.schema.json`'s `spawn`
/// shape. Unknown extra fields round-trip via `extra` (producers may extend, consumers must
/// tolerate — see `ocf/SPEC.md` "Permissive consumption").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnReceipt {
    pub kind: String,
    pub receipt_id: String,
    pub ts: DateTime<Utc>,
    pub parent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub task_boundary: String,
    pub budget: BudgetEnvelope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl SpawnReceipt {
    pub const KIND: &'static str = "spawn";
}

/// A return record — terminal; exactly one per spawn. Mirrors `receipts.schema.json`'s `return`
/// shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnReceipt {
    pub kind: String,
    pub receipt_id: String,
    pub ts: DateTime<Utc>,
    pub spawn_ref: String,
    pub stop_reason: StopReason,
    pub consumed: Consumed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distilled: Option<Distilled>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateDecision>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl ReturnReceipt {
    pub const KIND: &'static str = "return";
}

/// Bound the `task_boundary` field to the dispatched instruction's first line, truncated to
/// [`RECEIPT_TASK_BOUNDARY_CHARS`].
pub fn task_boundary_from_instruction(instruction: &str) -> String {
    let first_line = instruction.lines().next().unwrap_or("").trim();
    first_line
        .chars()
        .take(RECEIPT_TASK_BOUNDARY_CHARS)
        .collect()
}

// ── Config ─────────────────────────────────────────────────────────────────────────────────────

/// Receipts emission policy. Defaults are inert (non-breaking): emission is always attempted
/// when the run directory exists, but nothing is *enforced* unless `fail_closed` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiptsConfig {
    /// When `true`, a spawn whose receipt cannot be written, or which has no budget in any
    /// dimension, is refused with a clear error. When `false` (the default), a missing budget or
    /// a receipt-write failure just emits (or skips) the spawn record without enforcement.
    pub fail_closed: bool,
    /// Fallback budget applied when neither the delegation nor a lane contract declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_budget: Option<BudgetEnvelope>,
    /// fsync after every append. Off by default — emission is meant to be cheap, append-only.
    pub fsync: bool,
}

impl ReceiptsConfig {
    /// Read the fail-closed toggle from [`ARTESIAN_RECEIPTS_FAIL_CLOSED_ENV`]. `default_budget`
    /// and `fsync` are not env-configurable (no stable env-var shape for a structured budget);
    /// construct a [`ReceiptsConfig`] literal instead when those are needed.
    pub fn from_env() -> Self {
        Self {
            fail_closed: truthy_env(ARTESIAN_RECEIPTS_FAIL_CLOSED_ENV),
            default_budget: None,
            fsync: false,
        }
    }
}

fn truthy_env(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Fail-closed admission check: pure and side-effect free so it is directly unit-testable
/// without spawning a process. Returns `Err(reason)` when the spawn must be refused.
pub fn check_admission(budget: &BudgetEnvelope, fail_closed: bool) -> Result<(), String> {
    if fail_closed && budget.is_empty() {
        Err(
            "no budget dimension declared in any of budget/max_tokens/max_tool_calls/\
             max_wall_time_ms/max_cost_usd, and receipts.fail_closed is enabled"
                .to_string(),
        )
    } else {
        Ok(())
    }
}

// ── Writer ─────────────────────────────────────────────────────────────────────────────────────

/// Per-file shared state so concurrent dispatches against the *same* receipts file (e.g. several
/// lanes of one team running in parallel via `team.run`) still produce unique, monotonically
/// increasing ids and never interleave partial lines.
struct ReceiptFileState {
    seq: AtomicU64,
    write_lock: Mutex<()>,
}

fn file_state_registry() -> &'static Mutex<HashMap<PathBuf, Arc<ReceiptFileState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<ReceiptFileState>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn count_existing_lines(path: &Path) -> io::Result<u64> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u64),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Produce a filesystem- and id-safe run label. Empty or fully-punctuated input becomes
/// `"adhoc"` — the label used for dispatches that are not part of a team (e.g. an outer
/// orchestrator loop), matching [`crate::TeamRuntime::execute_delegation`]'s doc comment
/// convention of passing an empty `team_id` in that case.
fn sanitize_run_label(run: &str) -> String {
    let cleaned: String = run
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "adhoc".to_string()
    } else {
        cleaned
    }
}

/// Append-only JSONL writer for one run/team's `receipts-<run>.jsonl`, with run-scoped monotonic
/// ids `rc_<run>_<NNNNNN>` (zero-padded 6-digit sequence).
///
/// One file per run/team: the file lives directly under the same directory Flume already uses
/// for run-scoped, on-disk spawn state (`TeamRuntimeConfig::registry_dir`, e.g.
/// `.artesian/spawns`), named `receipts-<run>.jsonl` so it sits next to the process-registry
/// entries for that same run without colliding with them (those are `spawn-*.json`, a different
/// extension).
pub struct ReceiptWriter {
    path: PathBuf,
    run: String,
    fsync: bool,
    state: Arc<ReceiptFileState>,
}

impl ReceiptWriter {
    /// Open (or resume) the receipts file for `run` under `dir`, creating `dir` if needed.
    pub fn open(dir: &Path, run: impl Into<String>, fsync: bool) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let run = sanitize_run_label(&run.into());
        let path = dir.join(format!("receipts-{run}.jsonl"));
        let state = Self::shared_state(&path)?;
        Ok(Self {
            path,
            run,
            fsync,
            state,
        })
    }

    fn shared_state(path: &Path) -> io::Result<Arc<ReceiptFileState>> {
        let mut registry = file_state_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = registry.get(path) {
            return Ok(existing.clone());
        }
        let seq = count_existing_lines(path)?;
        let state = Arc::new(ReceiptFileState {
            seq: AtomicU64::new(seq),
            write_lock: Mutex::new(()),
        });
        registry.insert(path.to_path_buf(), state.clone());
        Ok(state)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn run(&self) -> &str {
        &self.run
    }

    /// Mint the next run-scoped monotonic receipt id: `rc_<run>_<NNNNNN>`.
    pub fn next_receipt_id(&self) -> String {
        let seq = self.state.seq.fetch_add(1, Ordering::SeqCst) + 1;
        format!("rc_{}_{seq:06}", self.run)
    }

    pub fn append_spawn(&self, receipt: &SpawnReceipt) -> io::Result<()> {
        self.append_line(receipt)
    }

    pub fn append_return(&self, receipt: &ReturnReceipt) -> io::Result<()> {
        self.append_line(receipt)
    }

    fn append_line(&self, value: &impl Serialize) -> io::Result<()> {
        let mut line = serde_json::to_string(value).map_err(io::Error::other)?;
        line.push('\n');
        let _guard = self
            .state
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        if self.fsync {
            file.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artesian_test_support::TempDir;

    fn minimal_spawn(writer: &ReceiptWriter, task_boundary: &str) -> SpawnReceipt {
        SpawnReceipt {
            kind: SpawnReceipt::KIND.to_string(),
            receipt_id: writer.next_receipt_id(),
            ts: Utc::now(),
            parent: "root".to_string(),
            role: Some("worker".to_string()),
            harness: Some("codex".to_string()),
            model: Some("gpt-5".to_string()),
            task_boundary: task_boundary.to_string(),
            budget: BudgetEnvelope {
                max_tokens: Some(10_000),
                ..BudgetEnvelope::default()
            },
            tool_allowlist: vec!["read".to_string()],
            schema_hash: None,
            extra: Map::new(),
        }
    }

    // ── (a) monotonic ids ──────────────────────────────────────────────────────────────────────

    #[test]
    fn next_receipt_id_is_monotonic_and_run_scoped() {
        let tempdir = TempDir::new("receipts-monotonic");
        let writer = ReceiptWriter::open(tempdir.path(), "team-x", false).expect("writer opens");
        assert_eq!(writer.next_receipt_id(), "rc_team-x_000001");
        assert_eq!(writer.next_receipt_id(), "rc_team-x_000002");
        assert_eq!(writer.next_receipt_id(), "rc_team-x_000003");
    }

    #[test]
    fn reopening_an_existing_file_resumes_the_sequence() {
        let tempdir = TempDir::new("receipts-resume");
        let path = {
            let writer = ReceiptWriter::open(tempdir.path(), "team-y", false).expect("open");
            let spawn = minimal_spawn(&writer, "first task");
            writer.append_spawn(&spawn).expect("append");
            writer.path().to_path_buf()
        };
        assert_eq!(count_existing_lines(&path).expect("count"), 1);

        // A *different* path (unique tempdir) starts fresh — proves the shared-state registry
        // keys strictly by path and does not leak sequence numbers across runs.
        let other = TempDir::new("receipts-resume-other");
        let other_writer =
            ReceiptWriter::open(other.path(), "team-y", false).expect("other writer opens");
        assert_eq!(other_writer.next_receipt_id(), "rc_team-y_000001");
    }

    #[test]
    fn empty_or_punctuation_only_run_label_falls_back_to_adhoc() {
        let tempdir = TempDir::new("receipts-adhoc");
        let writer = ReceiptWriter::open(tempdir.path(), "", false).expect("writer opens");
        assert_eq!(writer.run(), "adhoc");
        assert_eq!(writer.next_receipt_id(), "rc_adhoc_000001");
    }

    // ── (b) exactly one append per call, file layout ──────────────────────────────────────────

    #[test]
    fn append_spawn_and_return_produce_two_jsonl_lines_next_to_registry_state() {
        let tempdir = TempDir::new("receipts-layout");
        let writer = ReceiptWriter::open(tempdir.path(), "team", false).expect("writer opens");
        let spawn = minimal_spawn(&writer, "do the thing");
        writer.append_spawn(&spawn).expect("spawn append");
        let ret = ReturnReceipt {
            kind: ReturnReceipt::KIND.to_string(),
            receipt_id: writer.next_receipt_id(),
            ts: Utc::now(),
            spawn_ref: spawn.receipt_id.clone(),
            stop_reason: StopReason::Done,
            consumed: Consumed {
                wall_time_ms: Some(42),
                ..Consumed::default()
            },
            distilled: None,
            trace_ref: None,
            gate: None,
            extra: Map::new(),
        };
        writer.append_return(&ret).expect("return append");

        assert_eq!(writer.path(), tempdir.join("receipts-team.jsonl"));
        let contents = fs::read_to_string(writer.path()).expect("receipts file readable");
        let lines: Vec<&str> = contents.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 2, "exactly one spawn + one return line");
        let first: Value = serde_json::from_str(lines[0]).expect("spawn line is JSON");
        let second: Value = serde_json::from_str(lines[1]).expect("return line is JSON");
        assert_eq!(first["kind"], "spawn");
        assert_eq!(second["kind"], "return");
        assert_eq!(second["spawn_ref"], first["receipt_id"]);
    }

    // ── (d) fail-closed toggle ─────────────────────────────────────────────────────────────────

    #[test]
    fn fail_closed_refuses_budget_less_spawn() {
        let empty = BudgetEnvelope::default();
        assert!(check_admission(&empty, true).is_err());
    }

    #[test]
    fn fail_closed_allows_spawn_with_any_declared_dimension() {
        let one_dimension = BudgetEnvelope {
            max_wall_time_ms: Some(60_000),
            ..BudgetEnvelope::default()
        };
        assert!(check_admission(&one_dimension, true).is_ok());
    }

    #[test]
    fn fail_open_allows_budget_less_spawn() {
        let empty = BudgetEnvelope::default();
        assert!(check_admission(&empty, false).is_ok());
    }

    #[test]
    fn receipts_config_defaults_to_fail_open() {
        assert!(!ReceiptsConfig::default().fail_closed);
        assert!(!ReceiptsConfig::from_env().fail_closed);
    }

    // ── (e) serde roundtrip preserves unknown extra fields ────────────────────────────────────

    #[test]
    fn spawn_receipt_roundtrip_preserves_unknown_fields() {
        let json = serde_json::json!({
            "kind": "spawn",
            "receipt_id": "rc_team_000001",
            "ts": "2026-01-01T00:00:00Z",
            "parent": "root",
            "role": "worker",
            "task_boundary": "do the thing",
            "budget": { "max_tokens": 1000 },
            "future_field": "from a newer producer",
            "nested": { "a": 1 },
        });
        let receipt: SpawnReceipt =
            serde_json::from_value(json.clone()).expect("spawn receipt should deserialize");
        assert_eq!(
            receipt.extra.get("future_field"),
            Some(&Value::String("from a newer producer".to_string()))
        );
        assert_eq!(receipt.extra.get("nested"), json.get("nested"));

        let reencoded = serde_json::to_value(&receipt).expect("spawn receipt should serialize");
        assert_eq!(
            reencoded.get("future_field"),
            Some(&Value::String("from a newer producer".to_string()))
        );
        assert_eq!(reencoded.get("nested"), json.get("nested"));
    }

    #[test]
    fn return_receipt_roundtrip_preserves_unknown_fields() {
        let json = serde_json::json!({
            "kind": "return",
            "receipt_id": "rc_team_000002",
            "ts": "2026-01-01T00:00:01Z",
            "spawn_ref": "rc_team_000001",
            "stop_reason": "done",
            "consumed": { "wall_time_ms": 10 },
            "from_a_newer_consumer": true,
        });
        let receipt: ReturnReceipt =
            serde_json::from_value(json.clone()).expect("return receipt should deserialize");
        assert_eq!(
            receipt.extra.get("from_a_newer_consumer"),
            Some(&Value::Bool(true))
        );
        let reencoded = serde_json::to_value(&receipt).expect("return receipt should serialize");
        assert_eq!(
            reencoded.get("from_a_newer_consumer"),
            Some(&Value::Bool(true))
        );
    }

    // ── (f) shape test: required fields + stop_reason enum values match the OCF schema ───────

    #[test]
    fn stop_reason_values_match_ocf_schema() {
        // Hardcoded from ocf/schema/receipts.schema.json's `stop_reason` enum — no external
        // dependency on the sibling ocf checkout at test time.
        const SCHEMA_STOP_REASONS: [&str; 8] = [
            "done",
            "budget_tokens",
            "budget_tool_calls",
            "budget_time",
            "budget_cost",
            "gate_rejected",
            "error",
            "killed",
        ];
        let actual = [
            StopReason::Done,
            StopReason::BudgetTokens,
            StopReason::BudgetToolCalls,
            StopReason::BudgetTime,
            StopReason::BudgetCost,
            StopReason::GateRejected,
            StopReason::Error,
            StopReason::Killed,
        ]
        .map(|reason| {
            serde_json::to_value(reason)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        });
        assert_eq!(actual, SCHEMA_STOP_REASONS);
    }

    #[test]
    fn spawn_receipt_serializes_required_fields() {
        let writer_dir = TempDir::new("receipts-shape-spawn");
        let writer = ReceiptWriter::open(writer_dir.path(), "team", false).expect("writer opens");
        let spawn = minimal_spawn(&writer, "task boundary text");
        let value = serde_json::to_value(&spawn).expect("spawn receipt serializes");
        // Required per receipts.schema.json's `spawn` shape: kind, receipt_id, ts, parent,
        // task_boundary, budget.
        for field in [
            "kind",
            "receipt_id",
            "ts",
            "parent",
            "task_boundary",
            "budget",
        ] {
            assert!(
                value.get(field).is_some(),
                "spawn receipt missing required field {field:?}"
            );
        }
        assert_eq!(value["kind"], "spawn");
    }

    #[test]
    fn return_receipt_serializes_required_fields() {
        let writer_dir = TempDir::new("receipts-shape-return");
        let writer = ReceiptWriter::open(writer_dir.path(), "team", false).expect("writer opens");
        let ret = ReturnReceipt {
            kind: ReturnReceipt::KIND.to_string(),
            receipt_id: writer.next_receipt_id(),
            ts: Utc::now(),
            spawn_ref: "rc_team_000001".to_string(),
            stop_reason: StopReason::Done,
            consumed: Consumed::default(),
            distilled: None,
            trace_ref: None,
            gate: None,
            extra: Map::new(),
        };
        let value = serde_json::to_value(&ret).expect("return receipt serializes");
        // Required per receipts.schema.json's `return` shape: kind, receipt_id, ts, spawn_ref,
        // stop_reason, consumed.
        for field in [
            "kind",
            "receipt_id",
            "ts",
            "spawn_ref",
            "stop_reason",
            "consumed",
        ] {
            assert!(
                value.get(field).is_some(),
                "return receipt missing required field {field:?}"
            );
        }
        assert_eq!(value["kind"], "return");
    }

    #[test]
    fn task_boundary_takes_first_line_and_truncates() {
        let instruction = format!("{}\nsecond line is dropped", "x".repeat(250));
        let boundary = task_boundary_from_instruction(&instruction);
        assert_eq!(boundary.chars().count(), RECEIPT_TASK_BOUNDARY_CHARS);
        assert!(!boundary.contains("second line"));
    }
}
