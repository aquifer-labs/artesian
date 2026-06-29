// SPDX-License-Identifier: Apache-2.0

//! Token-savings statistics: how many tokens targeted recall saves vs loading the full
//! source records.
//!
//! ## Baseline assumption
//!
//! `baseline_tokens` is the sum of `count_tokens(record.content)` for every source record
//! that contributed a hit in the recall response.  The `returned_tokens` is the token count
//! of the actual response payload (possibly truncated or formatted differently).
//! `saved_tokens = max(0, baseline_tokens - returned_tokens)`.
//!
//! **Where savings come from by operation**
//!
//! | Operation | Baseline | Returned | Typical saving |
//! |-----------|---------|----------|----------------|
//! | `loop.recall` | full record content for each MMR-selected hit | 280-char truncated lines | significant when records are long |
//! | `memory.context` | full `index.md` content + full hit record content | truncated index slice + hit content | meaningful when index.md > `index_chars` limit |
//! | `memory.find` | full record content for returned hits | same (full content) | ~0 (no truncation) |
//! | `memory.session.resume` | resume packet tokens | same | ~0 (full packet returned) |
//!
//! This is a **conservative** baseline: it never counts the whole corpus, only the records
//! that were actually retrieved.  Cap is always `max(0, ...)` so saved_tokens is never
//! negative.

use std::{
    collections::HashMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SAVINGS_LOG: &str = "token_savings.jsonl";
const SAVINGS_ROLLUP: &str = "token_savings.json";

/// Environment variable that overrides the default `~/.artesian` statistics directory.
pub const ARTESIAN_STATS_DIR_ENV: &str = "ARTESIAN_STATS_DIR";

/// Resolve the statistics directory.
///
/// Checks `ARTESIAN_STATS_DIR` first; falls back to `~/.artesian`.
pub fn stats_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(ARTESIAN_STATS_DIR_ENV) {
        return PathBuf::from(dir);
    }
    #[allow(deprecated)]
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".artesian")
}

/// One token-savings measurement appended as a JSON line to `token_savings.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSavingsEntry {
    /// UTC timestamp of the recall.
    pub ts: DateTime<Utc>,
    /// Operation name: `memory.find`, `memory.context`, `memory.session.resume`,
    /// `loop.recall`, etc.
    pub op: String,
    /// Memory collection label from the backend config.
    pub collection: String,
    /// Tokens in the slice actually returned to the agent.
    pub returned_tokens: usize,
    /// Tokens in the full source records the returned hits came from.
    pub baseline_tokens: usize,
    /// `max(0, baseline_tokens - returned_tokens)`.
    pub saved_tokens: usize,
    /// Optional agent/session identifier for per-session ROI accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Estimated per-judge prompt/completion tokens when the operation ran a judge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge_costs: Vec<JudgeTokenCost>,
}

impl TokenSavingsEntry {
    pub fn new(op: &str, collection: &str, returned_tokens: usize, baseline_tokens: usize) -> Self {
        Self::with_metadata(
            op,
            collection,
            returned_tokens,
            baseline_tokens,
            TokenSavingsMetadata::default(),
        )
    }

    pub fn with_metadata(
        op: &str,
        collection: &str,
        returned_tokens: usize,
        baseline_tokens: usize,
        metadata: TokenSavingsMetadata,
    ) -> Self {
        let saved_tokens = baseline_tokens.saturating_sub(returned_tokens);
        Self {
            ts: Utc::now(),
            op: op.to_owned(),
            collection: collection.to_owned(),
            returned_tokens,
            baseline_tokens,
            saved_tokens,
            session_id: metadata.session_id,
            judge_costs: metadata.judge_costs,
        }
    }
}

/// Optional metadata attached to a token-savings entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSavingsMetadata {
    pub session_id: Option<String>,
    pub judge_costs: Vec<JudgeTokenCost>,
}

impl TokenSavingsMetadata {
    pub fn session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            judge_costs: Vec::new(),
        }
    }

    pub fn with_judge_costs(mut self, judge_costs: Vec<JudgeTokenCost>) -> Self {
        self.judge_costs = judge_costs;
        self
    }
}

/// Estimated token cost for one judge invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeTokenCost {
    pub judge: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

impl JudgeTokenCost {
    pub fn new(judge: impl Into<String>, prompt_tokens: usize, completion_tokens: usize) -> Self {
        let total_tokens = prompt_tokens.saturating_add(completion_tokens);
        Self {
            judge: judge.into(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
        }
    }
}

/// Per-operation totals inside the rollup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpSavings {
    pub calls: u64,
    pub returned_total: u64,
    pub baseline_total: u64,
    pub saved_total: u64,
}

/// Per-judge token-cost totals inside the rollup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeCostSavings {
    pub calls: u64,
    pub prompt_total: u64,
    pub completion_total: u64,
    pub total_tokens: u64,
}

/// Compact rollup written to `token_savings.json` and updated on every recall.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSavingsRollup {
    pub calls: u64,
    pub returned_total: u64,
    pub baseline_total: u64,
    pub saved_total: u64,
    pub by_op: HashMap<String, OpSavings>,
    /// Token-savings totals partitioned by `session_id`, when entries provide one.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_session: HashMap<String, OpSavings>,
    /// Estimated judge token cost across measured operations.
    #[serde(default)]
    pub judge_cost_total: u64,
    /// Estimated per-judge prompt/completion/total token cost.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_judge: HashMap<String, JudgeCostSavings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_ts: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ts: Option<DateTime<Utc>>,
}

// ── Internal directory-aware helpers (also used by tests) ────────────────────────────────────

/// Write a savings entry to `dir`.  All I/O errors are silently swallowed — a stats failure
/// must never fail or slow a recall.
pub(crate) fn record_savings_to_dir(
    dir: &Path,
    op: &str,
    collection: &str,
    returned_tokens: usize,
    baseline_tokens: usize,
) {
    record_savings_to_dir_with_metadata(
        dir,
        op,
        collection,
        returned_tokens,
        baseline_tokens,
        TokenSavingsMetadata::default(),
    );
}

pub(crate) fn record_savings_to_dir_with_metadata(
    dir: &Path,
    op: &str,
    collection: &str,
    returned_tokens: usize,
    baseline_tokens: usize,
    metadata: TokenSavingsMetadata,
) {
    let entry = TokenSavingsEntry::with_metadata(
        op,
        collection,
        returned_tokens,
        baseline_tokens,
        metadata,
    );
    let _ = try_write_savings(dir, &entry);
}

fn try_write_savings(dir: &Path, entry: &TokenSavingsEntry) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;

    // Append one JSON line to the append-only log.
    let log_path = dir.join(SAVINGS_LOG);
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)?;
    writeln!(f, "{line}")?;

    // Read-modify-write the compact rollup so CLI queries are O(1).
    let rollup_path = dir.join(SAVINGS_ROLLUP);
    let mut rollup = try_read_rollup(&rollup_path).unwrap_or_default();
    apply_entry_to_rollup(&mut rollup, entry);

    let json = serde_json::to_string_pretty(&rollup).map_err(std::io::Error::other)?;
    fs::write(&rollup_path, json.as_bytes())?;

    Ok(())
}

fn apply_entry_to_rollup(rollup: &mut TokenSavingsRollup, entry: &TokenSavingsEntry) {
    rollup.calls += 1;
    rollup.returned_total += entry.returned_tokens as u64;
    rollup.baseline_total += entry.baseline_tokens as u64;
    rollup.saved_total += entry.saved_tokens as u64;
    if rollup.first_ts.is_none() {
        rollup.first_ts = Some(entry.ts);
    }
    rollup.last_ts = Some(entry.ts);

    let op_entry = rollup.by_op.entry(entry.op.clone()).or_default();
    accumulate_savings(
        op_entry,
        entry.returned_tokens,
        entry.baseline_tokens,
        entry.saved_tokens,
    );

    if let Some(session_id) = entry
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
    {
        let session_entry = rollup.by_session.entry(session_id.to_string()).or_default();
        accumulate_savings(
            session_entry,
            entry.returned_tokens,
            entry.baseline_tokens,
            entry.saved_tokens,
        );
    }

    for cost in &entry.judge_costs {
        rollup.judge_cost_total += cost.total_tokens as u64;
        let judge = rollup.by_judge.entry(cost.judge.clone()).or_default();
        judge.calls += 1;
        judge.prompt_total += cost.prompt_tokens as u64;
        judge.completion_total += cost.completion_tokens as u64;
        judge.total_tokens += cost.total_tokens as u64;
    }
}

fn accumulate_savings(
    savings: &mut OpSavings,
    returned_tokens: usize,
    baseline_tokens: usize,
    saved_tokens: usize,
) {
    savings.calls += 1;
    savings.returned_total += returned_tokens as u64;
    savings.baseline_total += baseline_tokens as u64;
    savings.saved_total += saved_tokens as u64;
}

fn try_read_rollup(path: &Path) -> Option<TokenSavingsRollup> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Load the rollup from `dir`, re-aggregating from the JSONL log when `since` is set.
pub(crate) fn load_rollup_from_dir(dir: &Path, since: Option<DateTime<Utc>>) -> TokenSavingsRollup {
    if since.is_none() {
        // Fast path: return the precomputed compact rollup.
        let rollup_path = dir.join(SAVINGS_ROLLUP);
        if let Some(rollup) = try_read_rollup(&rollup_path) {
            return rollup;
        }
    }
    // Slow path: re-aggregate from the JSONL log for filtered queries.
    aggregate_from_log(&dir.join(SAVINGS_LOG), since)
}

fn aggregate_from_log(path: &Path, since: Option<DateTime<Utc>>) -> TokenSavingsRollup {
    let Ok(content) = fs::read_to_string(path) else {
        return TokenSavingsRollup::default();
    };
    let mut rollup = TokenSavingsRollup::default();
    for raw in content.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<TokenSavingsEntry>(raw) else {
            continue;
        };
        if let Some(since) = since {
            if entry.ts < since {
                continue;
            }
        }
        apply_entry_to_rollup(&mut rollup, &entry);
    }
    rollup
}

// ── Public API ────────────────────────────────────────────────────────────────────────────────

/// Record one token-savings measurement.  Best-effort: any I/O error is silently swallowed
/// so stats failures never fail or slow a recall.
///
/// `track = false` (set via `config.memory.track_savings = false`) skips recording entirely.
/// The statistics directory is resolved by [`stats_dir`] (env `ARTESIAN_STATS_DIR` →
/// `~/.artesian`).
pub fn record_savings(
    op: &str,
    collection: &str,
    returned_tokens: usize,
    baseline_tokens: usize,
    track: bool,
) {
    if !track {
        return;
    }
    record_savings_to_dir(
        &stats_dir(),
        op,
        collection,
        returned_tokens,
        baseline_tokens,
    );
}

/// Record one token-savings measurement with optional session and judge-cost metadata.
pub fn record_savings_with_metadata(
    op: &str,
    collection: &str,
    returned_tokens: usize,
    baseline_tokens: usize,
    metadata: TokenSavingsMetadata,
    track: bool,
) {
    if !track {
        return;
    }
    record_savings_to_dir_with_metadata(
        &stats_dir(),
        op,
        collection,
        returned_tokens,
        baseline_tokens,
        metadata,
    );
}

/// Load the cumulative token-savings rollup from the configured statistics directory.
///
/// When `since` is `Some`, re-aggregates from the JSONL log to apply the time filter;
/// otherwise returns the precomputed compact rollup instantly.
pub fn load_savings_rollup(since: Option<DateTime<Utc>>) -> TokenSavingsRollup {
    load_rollup_from_dir(&stats_dir(), since)
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_dir() -> PathBuf {
        let mut path = std::env::temp_dir();
        // Include pid + a counter to isolate parallel test runs.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        path.push(format!("artesian-savings-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn entry_fields_correct() {
        let entry = TokenSavingsEntry::new("memory.find", "artesian-memory", 40, 200);
        assert_eq!(entry.op, "memory.find");
        assert_eq!(entry.collection, "artesian-memory");
        assert_eq!(entry.returned_tokens, 40);
        assert_eq!(entry.baseline_tokens, 200);
        assert_eq!(entry.saved_tokens, 160);
        assert!(entry.session_id.is_none());
        assert!(entry.judge_costs.is_empty());
        // Timestamp is recent (within 10 seconds).
        let age = Utc::now().signed_duration_since(entry.ts);
        assert!(age.num_seconds() < 10);
    }

    #[test]
    fn saved_tokens_saturates_at_zero() {
        let entry = TokenSavingsEntry::new("memory.find", "col", 500, 100);
        assert_eq!(
            entry.saved_tokens, 0,
            "returned > baseline must not underflow"
        );
    }

    #[test]
    fn rollup_aggregates_multiple_recalls() {
        let dir = make_temp_dir();

        record_savings_to_dir(&dir, "memory.find", "col", 30, 300);
        record_savings_to_dir(&dir, "memory.context", "col", 20, 80);

        let rollup = load_rollup_from_dir(&dir, None);
        assert_eq!(rollup.calls, 2);
        assert_eq!(rollup.returned_total, 50);
        assert_eq!(rollup.baseline_total, 380);
        assert_eq!(rollup.saved_total, 330, "270 + 60");

        let find_op = rollup
            .by_op
            .get("memory.find")
            .expect("memory.find in by_op");
        assert_eq!(find_op.calls, 1);
        assert_eq!(find_op.saved_total, 270);

        let ctx_op = rollup
            .by_op
            .get("memory.context")
            .expect("memory.context in by_op");
        assert_eq!(ctx_op.calls, 1);
        assert_eq!(ctx_op.saved_total, 60);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn since_filter_excludes_older_entries() {
        let dir = make_temp_dir();

        // Manually write an old entry directly to the JSONL log.
        let old_entry = TokenSavingsEntry {
            ts: DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            op: "memory.find".to_owned(),
            collection: "col".to_owned(),
            returned_tokens: 10,
            baseline_tokens: 100,
            saved_tokens: 90,
            session_id: None,
            judge_costs: Vec::new(),
        };
        let log_path = dir.join(SAVINGS_LOG);
        std::fs::write(
            &log_path,
            format!("{}\n", serde_json::to_string(&old_entry).unwrap()),
        )
        .unwrap();

        // Write a recent entry via the normal path.
        record_savings_to_dir(&dir, "loop.recall", "col", 5, 50);

        let since = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let rollup = load_rollup_from_dir(&dir, Some(since));
        assert_eq!(rollup.calls, 1, "only the recent entry should be counted");
        assert_eq!(rollup.by_op.get("loop.recall").map(|o| o.calls), Some(1));
        assert!(
            !rollup.by_op.contains_key("memory.find"),
            "old entry excluded"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_write_failure_does_not_panic() {
        // Path underneath /dev/null cannot be a directory — write must fail silently.
        let bad_dir = PathBuf::from("/dev/null/artesian-savings-cannot-exist");
        // Must not panic or return an error.
        record_savings_to_dir(&bad_dir, "memory.find", "col", 10, 100);
    }

    #[test]
    fn json_shape_has_required_fields() {
        let rollup = TokenSavingsRollup {
            calls: 3,
            saved_total: 150,
            by_session: HashMap::new(),
            by_judge: HashMap::new(),
            by_op: [(
                "memory.find".to_owned(),
                OpSavings {
                    calls: 3,
                    saved_total: 150,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let json = serde_json::to_string(&rollup).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("calls").is_some(), "calls field");
        assert!(v.get("saved_total").is_some(), "saved_total field");
        assert!(v.get("by_op").is_some(), "by_op field");
        assert!(
            v.get("judge_cost_total").is_some(),
            "judge_cost_total field"
        );
        assert!(v["by_op"]["memory.find"]["calls"].as_u64() == Some(3));
    }

    #[test]
    fn returns_empty_rollup_when_no_data() {
        let dir = make_temp_dir();
        let rollup = load_rollup_from_dir(&dir, None);
        assert_eq!(rollup.calls, 0);
        assert_eq!(rollup.saved_total, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollup_tracks_session_and_judge_costs() {
        let dir = make_temp_dir();
        let metadata = TokenSavingsMetadata::session("session-a").with_judge_costs(vec![
            JudgeTokenCost::new("cheap", 11, 3),
            JudgeTokenCost::new("frontier", 17, 5),
        ]);

        record_savings_to_dir_with_metadata(&dir, "qualify.reject", "col", 0, 80, metadata);

        let rollup = load_rollup_from_dir(&dir, None);
        assert_eq!(rollup.saved_total, 80);
        assert_eq!(rollup.by_session["session-a"].saved_total, 80);
        assert_eq!(rollup.judge_cost_total, 36);
        assert_eq!(rollup.by_judge["cheap"].total_tokens, 14);
        assert_eq!(rollup.by_judge["frontier"].completion_total, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
