// SPDX-License-Identifier: Apache-2.0

//! Emit a small OCF `receipts.jsonl` stream with flume's receipt writer.
//!
//! Cross-tool proof: the file this example writes validates with the OpenHavn CLI —
//! `openhavn receipts validate <printed path>` — because both sides implement the same
//! OCF lifecycle-receipts spec (ocf/SPEC.md section 5).
//!
//! Usage: `cargo run -p flume --example emit_receipts [out_dir]`

use std::env;

use chrono::Utc;
use flume::{
    task_boundary_from_instruction, BudgetEnvelope, Consumed, Distilled, GateDecision,
    ReceiptWriter, ReturnReceipt, SpawnReceipt, StopReason,
};
use serde_json::Map;

fn main() -> std::io::Result<()> {
    let out_dir = env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let writer = ReceiptWriter::open(&out_dir, "example", false)?;

    let orchestrator_id = writer.next_receipt_id();
    writer.append_spawn(&SpawnReceipt {
        kind: SpawnReceipt::KIND.to_string(),
        receipt_id: orchestrator_id.clone(),
        ts: Utc::now(),
        parent: "root".to_string(),
        role: Some("orchestrator".to_string()),
        harness: Some("claude-code".to_string()),
        model: None,
        task_boundary: task_boundary_from_instruction("Ship the auth refactor end to end"),
        budget: BudgetEnvelope {
            max_tokens: Some(200_000),
            max_tool_calls: Some(40),
            ..BudgetEnvelope::default()
        },
        tool_allowlist: Vec::new(),
        schema_hash: None,
        extra: Map::new(),
    })?;

    let worker_id = writer.next_receipt_id();
    writer.append_spawn(&SpawnReceipt {
        kind: SpawnReceipt::KIND.to_string(),
        receipt_id: worker_id.clone(),
        ts: Utc::now(),
        parent: orchestrator_id,
        role: Some("worker".to_string()),
        harness: Some("codex".to_string()),
        model: Some("gpt-5.5".to_string()),
        task_boundary: task_boundary_from_instruction("Implement the token rotation module only"),
        budget: BudgetEnvelope {
            max_tokens: Some(80_000),
            max_tool_calls: Some(15),
            ..BudgetEnvelope::default()
        },
        tool_allowlist: vec!["read".into(), "edit".into(), "bash".into()],
        schema_hash: None,
        extra: Map::new(),
    })?;

    writer.append_return(&ReturnReceipt {
        kind: ReturnReceipt::KIND.to_string(),
        receipt_id: writer.next_receipt_id(),
        ts: Utc::now(),
        spawn_ref: worker_id,
        stop_reason: StopReason::Done,
        consumed: Consumed {
            tokens: Some(61_212),
            tool_calls: Some(12),
            ..Consumed::default()
        },
        distilled: Some(Distilled {
            tokens: Some(410),
            reference: Some("inline://worker-summary".to_string()),
        }),
        trace_ref: Some("file://runs/example/worker.jsonl".to_string()),
        gate: Some(GateDecision {
            admitted: true,
            reason: Some("schema-valid, within budget".to_string()),
        }),
        extra: Map::new(),
    })?;

    println!("{}", writer.path().display());
    Ok(())
}
