// SPDX-License-Identifier: Apache-2.0

//! Agentic benchmark: measure **memory-guides-action**, not just recall.
//!
//! The distinction matters (MemoryArena, arXiv:2602.16313): "not *can you recall attempt 12*,
//! but *given attempts 1–46, what do you do on 47*?" A system that saturates recall benchmarks
//! (LoCoMo / LongMemEval) can still fail here because it never had to *use* the memory to decide.
//!
//! ## Protocol (MemoryArena-style)
//!
//! 1. Sessions are played in order. Each session contributes `facts` to the agent's memory store.
//! 2. After all sessions, the agent is posed an *action query* — "given everything you know, what
//!    is your next step / decision?" — along with `correct_action` and `distractor_actions`.
//! 3. An ACC cycle builds the committed context from accumulated memory; the LLM must pick the
//!    correct action from the choices. Success = correct pick.
//!
//! This differs from QA eval (which only tests recall): the distractor actions are plausible
//! (the agent cannot guess without the memory), and the correct answer requires synthesising
//! *what was tried before* with *what the current state is*.
//!
//! ## Scale lane
//!
//! `AgentTask::scale_lane` records how many total tokens the accumulated sessions represent.
//! The agentic eval runner reports accuracy bucketed by scale so researchers can see where memory
//! guidance degrades — "almost nobody benchmarks 1M–10M tokens" (roadmap), so this is the
//! bucket we care most about.

#[cfg(feature = "llm")]
mod runner {
    use std::sync::Arc;

    use headgate::{
        count_tokens, Headgate, HeadgateConfig, LlmClient, LlmRequest, RecallItem, RecallStore,
        StaticRecallStore,
    };
    use serde::{Deserialize, Serialize};

    use super::{AgentTask, ScaleLane};

    /// Outcome of running one [`AgentTask`].
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct AgentTaskOutcome {
        pub id: String,
        pub chose_correct: bool,
        pub committed_tokens: usize,
        pub sessions_accumulated: usize,
        pub chosen_action: String,
        pub scale_lane: ScaleLane,
    }

    /// Aggregate results across a set of agentic tasks.
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct AgenticEvalSummary {
        pub dataset: String,
        pub tasks: usize,
        pub graded: usize,
        pub action_correct: usize,
        pub accuracy: f32,
        pub mean_committed_tokens: f32,
    }

    fn recall_items_from_sessions(task: &AgentTask) -> Vec<RecallItem> {
        let mut items = Vec::new();
        let mut index = 0usize;
        for session in &task.sessions {
            for fact in &session.facts {
                let score = 1.0 - (index as f32 * 0.001).min(0.5);
                items.push(RecallItem::new(
                    format!("s{}f{}", session.id, index),
                    fact.clone(),
                    score,
                ));
                index += 1;
            }
        }
        items
    }

    fn action_prompt(committed: &str, query: &str, choices: &[String]) -> String {
        let joined = choices
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}", i + 1, c))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "You must choose the best next action based ONLY on the context below. \
Reply with ONLY the number of your choice (1, 2, 3, …).\n\n\
Context:\n{committed}\n\nQuestion: {query}\n\nChoices:\n{joined}\n\nYour choice (number only):"
        )
    }

    fn parse_choice(reply: &str, choices: &[String]) -> Option<String> {
        let trimmed = reply.trim();
        if let Ok(n) = trimmed.parse::<usize>() {
            choices.get(n.saturating_sub(1)).cloned()
        } else {
            choices
                .iter()
                .find(|c| trimmed.to_lowercase().contains(&c.to_lowercase()))
                .cloned()
        }
    }

    /// Run one agentic task: accumulate sessions → ACC cycle → ask for action → grade.
    pub async fn run_agent_task(
        task: &AgentTask,
        client: &dyn LlmClient,
        config: HeadgateConfig,
    ) -> headgate::HeadgateResult<AgentTaskOutcome> {
        let all_items = recall_items_from_sessions(task);
        let committed_raw: usize = all_items
            .iter()
            .map(|item| count_tokens(&item.content))
            .sum();
        let recall: Arc<dyn RecallStore> = Arc::new(StaticRecallStore::new(all_items));

        let mut headgate = Headgate::new(recall, config);
        headgate.cycle(&task.query).await?;
        let committed = headgate.render();
        let committed_tokens = count_tokens(&committed);

        let mut choices = vec![task.correct_action.clone()];
        choices.extend(task.distractor_actions.iter().cloned());
        shuffle_choices(&mut choices, &task.id);

        let prompt = action_prompt(&committed, &task.query, &choices);
        let reply = client
            .complete(LlmRequest::new(prompt).with_temperature(0.0))
            .await?;
        let chosen = parse_choice(&reply, &choices).unwrap_or_else(|| reply.trim().to_string());
        let chose_correct = chosen.to_lowercase() == task.correct_action.to_lowercase();

        let _ = committed_raw;
        Ok(AgentTaskOutcome {
            id: task.id.clone(),
            chose_correct,
            committed_tokens,
            sessions_accumulated: task.sessions.len(),
            chosen_action: chosen,
            scale_lane: task.scale_lane(),
        })
    }

    /// Deterministic pseudo-shuffle to avoid always putting correct_action first.
    fn shuffle_choices(choices: &mut Vec<String>, seed: &str) {
        let h = seed.bytes().fold(5381u64, |acc, b| {
            acc.wrapping_mul(33).wrapping_add(b as u64)
        });
        let n = choices.len();
        if n < 2 {
            return;
        }
        let pivot = (h as usize) % n;
        choices.swap(0, pivot);
    }

    /// Run all agentic tasks, aggregate into a summary.
    pub async fn run_agentic_eval(
        dataset: impl Into<String>,
        tasks: &[AgentTask],
        client: &dyn LlmClient,
        config: HeadgateConfig,
    ) -> (AgenticEvalSummary, Vec<AgentTaskOutcome>) {
        let mut outcomes = Vec::new();
        let mut correct = 0usize;
        let mut committed_total = 0usize;
        for task in tasks {
            match run_agent_task(task, client, config.clone()).await {
                Ok(outcome) => {
                    if outcome.chose_correct {
                        correct += 1;
                    }
                    committed_total += outcome.committed_tokens;
                    outcomes.push(outcome);
                }
                Err(_) => continue,
            }
        }
        let graded = outcomes.len();
        let summary = AgenticEvalSummary {
            dataset: dataset.into(),
            tasks: tasks.len(),
            graded,
            action_correct: correct,
            accuracy: if graded == 0 {
                0.0
            } else {
                correct as f32 / graded as f32
            },
            mean_committed_tokens: if graded == 0 {
                0.0
            } else {
                committed_total as f32 / graded as f32
            },
        };
        (summary, outcomes)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::agentic::{AgentTask, TaskSession};
        use futures_util::{future::BoxFuture, FutureExt};
        use headgate::HeadgateResult;

        struct PickFirstClient;
        impl LlmClient for PickFirstClient {
            fn complete(&self, request: LlmRequest) -> BoxFuture<'_, HeadgateResult<String>> {
                let choices = request
                    .prompt
                    .lines()
                    .filter_map(|line| {
                        let trimmed = line.trim();
                        if trimmed.starts_with("1.") {
                            Some("1")
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap_or("1");
                let reply = choices.to_string();
                async move { Ok(reply) }.boxed()
            }
        }

        fn task() -> AgentTask {
            AgentTask {
                id: "t1".into(),
                sessions: vec![
                    TaskSession {
                        id: "s1".into(),
                        facts: vec![
                            "we tried approach A in session 1 and it failed".into(),
                            "the failure was caused by missing auth headers".into(),
                        ],
                    },
                    TaskSession {
                        id: "s2".into(),
                        facts: vec!["we tried approach B in session 2 and it succeeded".into()],
                    },
                ],
                query: "what should we do on the next attempt?".into(),
                correct_action: "continue with approach B".into(),
                distractor_actions: vec![
                    "retry approach A".into(),
                    "start over from scratch".into(),
                ],
            }
        }

        #[tokio::test]
        async fn agent_task_runs_and_grades() {
            let t = task();
            let outcome = run_agent_task(&t, &PickFirstClient, HeadgateConfig::default())
                .await
                .expect("task should run");
            assert_eq!(outcome.id, "t1");
            assert_eq!(outcome.sessions_accumulated, 2);
            assert!(outcome.committed_tokens > 0);
        }

        #[tokio::test]
        async fn agentic_eval_aggregates() {
            let tasks = vec![task(), task()];
            let (summary, outcomes) =
                run_agentic_eval("smoke", &tasks, &PickFirstClient, HeadgateConfig::default())
                    .await;
            assert_eq!(summary.tasks, 2);
            assert_eq!(summary.graded, 2);
            assert_eq!(outcomes.len(), 2);
        }
    }
}

use serde::{Deserialize, Serialize};

/// Scale bucket for the "1M–10M token" honest scale lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ScaleLane {
    /// < 10k tokens of accumulated session facts.
    Small,
    /// 10k – 100k tokens.
    Medium,
    /// 100k – 1M tokens.
    Large,
    /// 1M – 10M tokens (the "almost nobody benchmarks this" regime from the roadmap).
    XLarge,
    /// > 10M tokens.
    Extreme,
}

impl ScaleLane {
    pub fn from_tokens(tokens: usize) -> Self {
        match tokens {
            0..=9_999 => Self::Small,
            10_000..=99_999 => Self::Medium,
            100_000..=999_999 => Self::Large,
            1_000_000..=9_999_999 => Self::XLarge,
            _ => Self::Extreme,
        }
    }
}

impl std::fmt::Display for ScaleLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Small => write!(f, "<10k"),
            Self::Medium => write!(f, "10k–100k"),
            Self::Large => write!(f, "100k–1M"),
            Self::XLarge => write!(f, "1M–10M"),
            Self::Extreme => write!(f, ">10M"),
        }
    }
}

/// One session's contribution to accumulated memory in an agentic task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSession {
    pub id: String,
    /// Facts the agent observed / learned in this session.
    pub facts: Vec<String>,
}

/// An interdependent multi-session agentic task.
///
/// Sessions are played in order; each session's facts accumulate into the agent's memory. The
/// task query is posed after all sessions with a set of choices — correct and distractor — and
/// success means the agent, using only accumulated memory, picks the correct action.
///
/// This is the MemoryArena (arXiv:2602.16313) model: memory must guide the next decision, not
/// just surface a recalled fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: String,
    /// Sessions in chronological order; each provides evidence for later decisions.
    pub sessions: Vec<TaskSession>,
    /// The action query posed after all sessions: "given everything, what is your next step?"
    pub query: String,
    /// The correct next action.
    pub correct_action: String,
    /// Plausible but wrong alternative actions (the agent should not guess).
    pub distractor_actions: Vec<String>,
}

impl AgentTask {
    /// Total tokens across all session facts — used to classify into a [`ScaleLane`].
    pub fn total_fact_tokens(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|s| s.facts.iter())
            .map(|f| f.split_whitespace().count() * 4 / 3)
            .sum()
    }

    pub fn scale_lane(&self) -> ScaleLane {
        ScaleLane::from_tokens(self.total_fact_tokens())
    }
}

/// Parse an agentic task fixture file (newline-delimited JSON, one `AgentTask` per line,
/// or a JSON array).
pub fn load_agent_tasks(json: &str) -> Result<Vec<AgentTask>, serde_json::Error> {
    if let Ok(tasks) = serde_json::from_str::<Vec<AgentTask>>(json) {
        return Ok(tasks);
    }
    json.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<AgentTask>)
        .collect()
}

#[cfg(feature = "llm")]
pub use runner::{run_agent_task, run_agentic_eval, AgentTaskOutcome, AgenticEvalSummary};

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn scale_lane_buckets_correctly() {
        assert_eq!(ScaleLane::from_tokens(0), ScaleLane::Small);
        assert_eq!(ScaleLane::from_tokens(9_999), ScaleLane::Small);
        assert_eq!(ScaleLane::from_tokens(10_000), ScaleLane::Medium);
        assert_eq!(ScaleLane::from_tokens(1_000_000), ScaleLane::XLarge);
        assert_eq!(ScaleLane::from_tokens(10_000_001), ScaleLane::Extreme);
    }

    #[test]
    fn load_agent_tasks_parses_array() {
        let json = r#"[
          {
            "id": "t1",
            "sessions": [
              {"id": "s1", "facts": ["We tried Rust and it worked well."]}
            ],
            "query": "what language should we use next?",
            "correct_action": "continue with Rust",
            "distractor_actions": ["switch to Python", "use C++"]
          }
        ]"#;
        let tasks = load_agent_tasks(json).expect("parses");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].correct_action, "continue with Rust");
    }

    #[test]
    fn load_agent_tasks_parses_ndjson() {
        let ndjson = r#"{"id":"t1","sessions":[{"id":"s1","facts":["fact1"]}],"query":"q","correct_action":"A","distractor_actions":["B"]}
{"id":"t2","sessions":[{"id":"s1","facts":["fact2"]}],"query":"q2","correct_action":"C","distractor_actions":["D"]}"#;
        let tasks = load_agent_tasks(ndjson).expect("parses ndjson");
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn agent_task_classifies_scale_lane() {
        let small = AgentTask {
            id: "s".into(),
            sessions: vec![TaskSession {
                id: "s1".into(),
                facts: vec!["hello world".into()],
            }],
            query: "q".into(),
            correct_action: "A".into(),
            distractor_actions: vec![],
        };
        assert_eq!(small.scale_lane(), ScaleLane::Small);
    }
}
