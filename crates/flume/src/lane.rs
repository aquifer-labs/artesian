// SPDX-License-Identifier: Apache-2.0

//! Lane contracts for parallel specialist lanes in a Flume team.
//!
//! A [`Lane`] is a named specialist with a written contract that declares exactly what it owns,
//! what it must not do, the resources it is allowed to consume, and how work passes in and out.
//! Lanes run in parallel under the existing global concurrency cap (`max_concurrent_spawns`)
//! plus queue — the cap is never doubled, just split across lanes.
//!
//! The coordinator ([`LaneCoordinator`]) sits inside [`TeamRuntime`] and enforces two properties:
//!
//! 1. **Deduplication** — the same task (matched by id or canonical title) cannot be active in
//!    two lanes simultaneously.  A second request for an in-flight task is rejected with
//!    [`LaneError::DuplicateTask`].
//!
//! 2. **Handoff routing** — when a lane completes a task it posts a handoff summary that the
//!    coordinator delivers to the lane specified by the contract's `handoff_to` field.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ── Budget ─────────────────────────────────────────────────────────────────────────────────────

/// Resource budget for one lane.  Every limit is *opt-in*: `None` means unbounded.
/// The global `max_concurrent_spawns` on [`TeamRuntimeConfig`] is always respected regardless of
/// per-lane budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneBudget {
    /// Maximum number of tasks this lane may run concurrently (within the global cap).
    pub max_concurrent_tasks: Option<usize>,
    /// Maximum total turns (worker invocations) this lane may consume across all its tasks.
    pub max_turns: Option<u32>,
    /// Optional soft token cap (advisory — the runtime records usage but does not enforce).
    pub token_cap: Option<u64>,
}

impl LaneBudget {
    /// No limits — the lane inherits the global caps only.
    pub const fn unlimited() -> Self {
        Self {
            max_concurrent_tasks: None,
            max_turns: None,
            token_cap: None,
        }
    }
}

impl Default for LaneBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

// ── LaneContract ───────────────────────────────────────────────────────────────────────────────

/// The written contract for one specialist lane.
///
/// A contract makes the lane's scope legible to coordinators, other lanes, and human operators.
/// It is intentionally a *data type* — no behaviour — so it can be serialised, versioned, and
/// diffed alongside role definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneContract {
    /// Human-readable description of what this lane owns.
    pub owned_scope: String,
    /// What this lane must NOT do (non-goals prevent overlap with sibling lanes).
    pub non_goals: Vec<String>,
    /// The resource budget for this lane.
    pub budget: LaneBudget,
    /// The name of the lane (or `team.message` `to` target) to receive handoff summaries when
    /// this lane completes a task.  `None` means the handoff goes to the master/lead.
    pub handoff_to: Option<String>,
    /// Agent or tool names this lane is allowed to call.  Empty = no restriction beyond the
    /// global policy.
    pub allowed_tools: Vec<String>,
    /// Agent CLI this lane uses (matches a role-definition `agent` field or binding agent name).
    pub agent_constraint: Option<String>,
}

impl LaneContract {
    /// Minimal contract with just an owned scope and no other restrictions.
    pub fn minimal(owned_scope: impl Into<String>) -> Self {
        Self {
            owned_scope: owned_scope.into(),
            non_goals: Vec::new(),
            budget: LaneBudget::unlimited(),
            handoff_to: None,
            allowed_tools: Vec::new(),
            agent_constraint: None,
        }
    }
}

// ── Lane (public type) ─────────────────────────────────────────────────────────────────────────

/// A named specialist lane.  The combination of a stable name + contract is the unit the
/// coordinator tracks and enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    /// Stable unique name within the team (e.g. `"security"`, `"test-runner"`).
    pub name: String,
    /// The definition (role) assigned to this lane.
    pub definition: String,
    /// The written contract.
    pub contract: LaneContract,
}

impl Lane {
    pub fn new(
        name: impl Into<String>,
        definition: impl Into<String>,
        contract: LaneContract,
    ) -> Self {
        Self {
            name: name.into(),
            definition: definition.into(),
            contract,
        }
    }
}

// ── LaneError ──────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LaneError {
    #[error("lane '{0}' not found")]
    LaneNotFound(String),
    #[error("duplicate task: task '{task}' is already active in lane '{owner}'")]
    DuplicateTask { task: String, owner: String },
    #[error("lane budget exceeded: {0}")]
    BudgetExceeded(String),
}

// ── Internal coordinator state ─────────────────────────────────────────────────────────────────

/// Tracks per-task ownership within the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneTaskEntry {
    /// The task id (from the headrace task board).
    pub task_id: String,
    /// Canonical lowercase title used for dedup matching.
    pub canonical_title: String,
    /// The lane that owns this task.
    pub lane_name: String,
}

/// Runtime state for one lane inside the coordinator.
#[derive(Debug, Clone)]
pub(crate) struct LaneState {
    pub lane: Lane,
    /// Turn counter (worker invocations) across all tasks in this lane.
    pub turns_used: u32,
    /// Total advisory token usage.
    pub tokens_used: u64,
    /// Active task ids claimed by this lane (subset of coordinator's global map).
    pub active_task_ids: BTreeSet<String>,
}

impl LaneState {
    pub fn new(lane: Lane) -> Self {
        Self {
            lane,
            turns_used: 0,
            tokens_used: 0,
            active_task_ids: BTreeSet::new(),
        }
    }

    /// Whether this lane has capacity for one more concurrent task.
    pub fn has_task_capacity(&self) -> bool {
        match self.lane.contract.budget.max_concurrent_tasks {
            Some(cap) => self.active_task_ids.len() < cap,
            None => true,
        }
    }

    /// Whether this lane would exceed its turn budget on the next invocation.
    pub fn can_use_turn(&self) -> bool {
        match self.lane.contract.budget.max_turns {
            Some(cap) => self.turns_used < cap,
            None => true,
        }
    }
}

// ── LaneCoordinator ────────────────────────────────────────────────────────────────────────────

/// The lane coordinator lives inside [`TeamRuntime`] and enforces dedup + budget + handoff
/// routing across all lanes in a team.
///
/// It is intentionally *small* — no async, no I/O.  All concurrency primitives are handled by
/// the surrounding `TeamRuntime` which already holds a mutex over the full state.
#[derive(Debug, Default, Clone)]
pub struct LaneCoordinator {
    /// Lanes registered for this team, keyed by lane name.
    pub(crate) lanes: BTreeMap<String, LaneState>,
    /// Global task→lane ownership map for dedup checking.
    pub(crate) task_owners: Vec<LaneTaskEntry>,
}

impl LaneCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a lane.  Overwrites any prior registration with the same name (idempotent for
    /// re-configuration).
    pub fn register_lane(&mut self, lane: Lane) {
        self.lanes.insert(lane.name.clone(), LaneState::new(lane));
    }

    /// Attempt to assign `task_id` / `task_title` to `lane_name`.
    ///
    /// Returns [`LaneError::DuplicateTask`] if the task is already active in *any* lane (matched
    /// by id or by canonical title), so two lanes cannot race on the same work.
    /// Returns [`LaneError::BudgetExceeded`] if the lane's concurrent-task cap is full.
    /// Returns [`LaneError::LaneNotFound`] if the lane name is unknown.
    pub fn assign_task(
        &mut self,
        lane_name: &str,
        task_id: impl Into<String>,
        task_title: impl AsRef<str>,
    ) -> Result<(), LaneError> {
        let task_id = task_id.into();
        let canonical_title = canonical(task_title.as_ref());

        // 1. Dedup check against ALL lanes.
        if let Some(entry) = self
            .task_owners
            .iter()
            .find(|e| e.task_id == task_id || e.canonical_title == canonical_title)
        {
            return Err(LaneError::DuplicateTask {
                task: task_id,
                owner: entry.lane_name.clone(),
            });
        }

        // 2. Lane existence + budget check.
        let state = self
            .lanes
            .get_mut(lane_name)
            .ok_or_else(|| LaneError::LaneNotFound(lane_name.to_string()))?;

        if !state.has_task_capacity() {
            return Err(LaneError::BudgetExceeded(format!(
                "lane '{}' concurrent-task cap ({}) reached",
                lane_name,
                state.lane.contract.budget.max_concurrent_tasks.unwrap_or(0)
            )));
        }

        // 3. Assign.
        state.active_task_ids.insert(task_id.clone());
        self.task_owners.push(LaneTaskEntry {
            task_id,
            canonical_title,
            lane_name: lane_name.to_string(),
        });
        Ok(())
    }

    /// Mark a task complete in its lane, freeing the slot and returning the handoff target (if
    /// any) from the lane's contract.
    pub fn complete_task(
        &mut self,
        task_id: &str,
        tokens_used: u64,
    ) -> Option<(String, Option<String>)> {
        let idx = self.task_owners.iter().position(|e| e.task_id == task_id)?;
        let entry = self.task_owners.remove(idx);
        let state = self.lanes.get_mut(&entry.lane_name)?;
        state.active_task_ids.remove(task_id);
        state.turns_used += 1;
        state.tokens_used += tokens_used;
        let handoff_to = state.lane.contract.handoff_to.clone();
        Some((entry.lane_name, handoff_to))
    }

    /// Record one turn for a lane (used by the coordinator when a worker invocation completes).
    pub fn record_turn(&mut self, lane_name: &str) -> Result<(), LaneError> {
        let state = self
            .lanes
            .get_mut(lane_name)
            .ok_or_else(|| LaneError::LaneNotFound(lane_name.to_string()))?;
        if !state.can_use_turn() {
            return Err(LaneError::BudgetExceeded(format!(
                "lane '{}' turn budget ({}) exhausted",
                lane_name,
                state.lane.contract.budget.max_turns.unwrap_or(0)
            )));
        }
        state.turns_used += 1;
        Ok(())
    }

    /// Return all registered lane summaries (for presence / status reporting).
    pub fn lane_summaries(&self) -> Vec<LaneSummary> {
        self.lanes.values().map(LaneSummary::from_state).collect()
    }

    /// Owner lane name for a task id (for presence / dedup queries).
    pub fn owner_of(&self, task_id: &str) -> Option<&str> {
        self.task_owners
            .iter()
            .find(|e| e.task_id == task_id)
            .map(|e| e.lane_name.as_str())
    }
}

// ── LaneSummary (serialisable snapshot) ───────────────────────────────────────────────────────

/// A read-only snapshot of one lane's state, suitable for presence reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneSummary {
    pub name: String,
    pub definition: String,
    pub contract: LaneContract,
    pub active_task_ids: Vec<String>,
    pub turns_used: u32,
    pub tokens_used: u64,
}

impl LaneSummary {
    fn from_state(state: &LaneState) -> Self {
        Self {
            name: state.lane.name.clone(),
            definition: state.lane.definition.clone(),
            contract: state.lane.contract.clone(),
            active_task_ids: state.active_task_ids.iter().cloned().collect(),
            turns_used: state.turns_used,
            tokens_used: state.tokens_used,
        }
    }
}

// ── PresenceSnapshot ───────────────────────────────────────────────────────────────────────────

/// Live presence snapshot for a team — which lane/agent is active on what task right now.
/// Returned by [`TeamRuntime::presence`] and the `team.presence` MCP tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceSnapshot {
    pub team_id: String,
    /// Lane-level view (active tasks per lane, budget usage).
    pub lanes: Vec<LaneSummary>,
    /// Teammate-level view (reuses existing [`TeammatePresence`] type).
    pub teammates: Vec<TeammatePresence>,
    /// Total active tasks across all lanes.
    pub total_active_tasks: usize,
    /// Total load: active spawns / global cap.
    pub spawns_active: usize,
    pub spawns_cap: usize,
}

/// One teammate's current presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeammatePresence {
    pub name: String,
    pub lane: Option<String>,
    pub status: String,
    pub active_task_ids: Vec<String>,
}

// ── helpers ────────────────────────────────────────────────────────────────────────────────────

/// Produce a stable, case-folded key for dedup matching by title.
fn canonical(title: &str) -> String {
    title.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(name: &str) -> Lane {
        Lane::new(
            name,
            format!("{name}-worker"),
            LaneContract {
                owned_scope: format!("{name} tasks"),
                non_goals: Vec::new(),
                budget: LaneBudget {
                    max_concurrent_tasks: Some(2),
                    max_turns: Some(10),
                    token_cap: None,
                },
                handoff_to: Some("judge".to_string()),
                allowed_tools: Vec::new(),
                agent_constraint: None,
            },
        )
    }

    #[test]
    fn lane_contract_owns_scope_and_coordinator_refuses_duplicate_task() {
        let mut coord = LaneCoordinator::new();
        coord.register_lane(lane("security"));
        coord.register_lane(lane("test-runner"));

        // First assignment succeeds.
        coord
            .assign_task("security", "task-1", "Review auth.rs")
            .expect("first assign should succeed");

        // Same task_id in a different lane is rejected.
        let err = coord
            .assign_task("test-runner", "task-1", "Review auth.rs copy")
            .expect_err("duplicate task_id should be rejected");
        assert!(matches!(err, LaneError::DuplicateTask { .. }));

        // Same canonical title (different case) in a different lane is also rejected.
        let err2 = coord
            .assign_task("test-runner", "task-2", "review auth.rs")
            .expect_err("duplicate canonical title should be rejected");
        assert!(matches!(err2, LaneError::DuplicateTask { .. }));

        // A genuinely distinct task succeeds.
        coord
            .assign_task("test-runner", "task-99", "Run integration tests")
            .expect("distinct task should succeed");
    }

    #[test]
    fn completed_task_frees_slot_and_returns_handoff_target() {
        let mut coord = LaneCoordinator::new();
        coord.register_lane(lane("security"));
        coord
            .assign_task("security", "task-1", "Review auth.rs")
            .expect("assign should succeed");

        let result = coord.complete_task("task-1", 0);
        assert!(result.is_some());
        let (owner, handoff) = result.unwrap();
        assert_eq!(owner, "security");
        assert_eq!(handoff.as_deref(), Some("judge"));

        // Slot is free: the same task id can be assigned again (e.g. if re-queued).
        coord
            .assign_task("security", "task-1", "Review auth.rs")
            .expect("slot should be free after completion");
    }

    #[test]
    fn budget_cap_refuses_when_concurrent_task_limit_reached() {
        let mut coord = LaneCoordinator::new();
        // Lane with cap=1.
        coord.register_lane(Lane::new(
            "narrow",
            "narrow-worker",
            LaneContract {
                owned_scope: "narrow tasks".to_string(),
                non_goals: Vec::new(),
                budget: LaneBudget {
                    max_concurrent_tasks: Some(1),
                    max_turns: None,
                    token_cap: None,
                },
                handoff_to: None,
                allowed_tools: Vec::new(),
                agent_constraint: None,
            },
        ));

        coord
            .assign_task("narrow", "t1", "First task")
            .expect("first task should fit");
        let err = coord
            .assign_task("narrow", "t2", "Second task")
            .expect_err("second task should exceed cap");
        assert!(matches!(err, LaneError::BudgetExceeded(_)));
    }

    #[test]
    fn lane_summaries_reflect_active_tasks() {
        let mut coord = LaneCoordinator::new();
        coord.register_lane(lane("security"));
        coord
            .assign_task("security", "task-a", "Audit tokens")
            .expect("assign should succeed");

        let summaries = coord.lane_summaries();
        let sec = summaries
            .iter()
            .find(|s| s.name == "security")
            .expect("security lane should appear in summaries");
        assert_eq!(sec.active_task_ids, vec!["task-a"]);
    }
}
