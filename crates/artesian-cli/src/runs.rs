// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
struct RunSummary {
    run_id: String,
    started_at: Option<DateTime<Utc>>,
    sort_time: SystemTime,
    turns: u64,
    status: String,
    active: bool,
    path: PathBuf,
}

pub(crate) fn list(root: &Path) -> Result<()> {
    let runs = list_recent(root)?;
    if runs.is_empty() {
        println!("No runs found in {}", root.display());
        return Ok(());
    }

    println!(
        "{:<32} {:<20} {:>5}  status",
        "run_id", "started_at", "turns"
    );
    for run in runs {
        println!(
            "{:<32} {:<20} {:>5}  {}",
            run.run_id,
            format_timestamp(run.started_at),
            run.turns,
            run.status
        );
    }
    Ok(())
}

pub(crate) async fn watch(root: &Path, run_id: Option<&str>) -> Result<()> {
    let run = match run_id {
        Some(run_id) => RunSelection {
            run_id: run_id.to_string(),
            path: run_path(root, run_id),
        },
        None => select_default_run(root)?,
    };
    if !run.path.exists() {
        bail!("run log not found: {}", run.path.display());
    }

    println!("watching {} ({})", run.run_id, run.path.display());
    let mut file = File::open(&run.path).with_context(|| format!("open {}", run.path.display()))?;
    let mut offset = 0;
    let mut pending = String::new();

    loop {
        let lines = read_appended_lines(&mut file, &mut offset, &mut pending)
            .with_context(|| format!("read {}", run.path.display()))?;
        for line in lines {
            let record: Value = serde_json::from_str(&line)
                .with_context(|| format!("parse run-log line from {}", run.path.display()))?;
            if render_watch_record(&record)? {
                return Ok(());
            }
        }

        std::io::stdout().flush()?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("listen for ctrl-c")?;
                return Ok(());
            }
            () = tokio::time::sleep(WATCH_POLL_INTERVAL) => {}
        }
    }
}

fn list_recent(root: &Path) -> Result<Vec<RunSummary>> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut runs = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", root.display()))?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        runs.push(read_run_summary(&path)?);
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.sort_time));
    Ok(runs)
}

fn read_run_summary(path: &Path) -> Result<RunSummary> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let records = parse_complete_records(&text, path)?;
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let fallback_time = metadata
        .created()
        .or_else(|_| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let started_at = records
        .iter()
        .find_map(timestamp_from_record)
        .or_else(|| Some(DateTime::<Utc>::from(fallback_time)));
    let summary = records
        .iter()
        .find(|record| record_type(record) == Some("summary"));
    let turns_from_records = records
        .iter()
        .filter(|record| record_type(record) == Some("turn"))
        .filter_map(|record| record.get("turn").and_then(Value::as_u64))
        .max()
        .unwrap_or_else(|| {
            records
                .iter()
                .filter(|record| record_type(record) == Some("turn"))
                .count() as u64
        });
    let turns = summary
        .and_then(|record| record.get("turns").and_then(Value::as_u64))
        .unwrap_or(turns_from_records);
    let active = summary.is_none();
    let status = summary
        .and_then(|record| record.get("outcome").and_then(Value::as_str))
        .unwrap_or("active")
        .to_string();
    let run_id = records
        .iter()
        .find_map(|record| record.get("run_id").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| file_stem(path));

    Ok(RunSummary {
        run_id,
        started_at,
        sort_time: started_at.map(Into::into).unwrap_or(fallback_time),
        turns,
        status,
        active,
        path: path.to_path_buf(),
    })
}

fn parse_complete_records(text: &str, path: &Path) -> Result<Vec<Value>> {
    let mut records = Vec::new();
    let lines = text.lines().collect::<Vec<_>>();
    let has_trailing_newline = text.ends_with('\n');
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) if index + 1 == lines.len() && !has_trailing_newline => {
                break;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("parse run-log line from {}", path.display()));
            }
        }
    }
    Ok(records)
}

#[derive(Debug)]
struct RunSelection {
    run_id: String,
    path: PathBuf,
}

fn select_default_run(root: &Path) -> Result<RunSelection> {
    let runs = list_recent(root)?;
    let selected = runs
        .iter()
        .find(|run| run.active)
        .or_else(|| runs.first())
        .with_context(|| format!("no run logs found in {}", root.display()))?;
    Ok(RunSelection {
        run_id: selected.run_id.clone(),
        path: selected.path.clone(),
    })
}

fn run_path(root: &Path, run_id: &str) -> PathBuf {
    let path = Path::new(run_id);
    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        root.join(path)
    } else {
        root.join(format!("{run_id}.jsonl"))
    }
}

fn read_appended_lines(
    file: &mut File,
    offset: &mut u64,
    pending: &mut String,
) -> Result<Vec<String>> {
    let len = file.metadata()?.len();
    if len < *offset {
        *offset = 0;
        pending.clear();
    }
    if len == *offset {
        return Ok(Vec::new());
    }

    file.seek(SeekFrom::Start(*offset))?;
    let mut chunk = String::new();
    file.read_to_string(&mut chunk)?;
    *offset += chunk.len() as u64;
    pending.push_str(&chunk);

    let mut lines = Vec::new();
    while let Some(newline) = pending.find('\n') {
        let line = pending[..newline].trim_end_matches('\r').to_string();
        pending.drain(..=newline);
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    Ok(lines)
}

fn render_watch_record(record: &Value) -> Result<bool> {
    match record_type(record) {
        Some("turn") => {
            let turn = record.get("turn").and_then(Value::as_u64).unwrap_or(0);
            let action = inline_text(record.get("action").and_then(Value::as_str).unwrap_or(""));
            let verify = verify_status(record);
            println!("turn {turn}  action: {action}  -> verify: {verify}");
            Ok(false)
        }
        Some("summary") => {
            let outcome = record
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let turns = record.get("turns").and_then(Value::as_u64).unwrap_or(0);
            let why = inline_text(
                record
                    .get("why_stopped")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            println!("summary  outcome: {outcome}  turns: {turns}  why: {why}");
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn verify_status(record: &Value) -> &'static str {
    if let Some(passed) = record
        .get("verify")
        .and_then(|verify| verify.get("passed"))
        .and_then(Value::as_bool)
    {
        return if passed { "passed" } else { "failed" };
    }
    match record.get("verify_result").and_then(Value::as_str) {
        Some("passed") | Some("success") => "passed",
        Some("failed") | Some("failure") => "failed",
        _ => "unknown",
    }
}

fn record_type(record: &Value) -> Option<&str> {
    record.get("type").and_then(Value::as_str)
}

fn timestamp_from_record(record: &Value) -> Option<DateTime<Utc>> {
    ["started_at", "start_time", "timestamp", "ts", "time"]
        .into_iter()
        .filter_map(|key| record.get(key).and_then(Value::as_str))
        .find_map(|value| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        })
}

fn format_timestamp(timestamp: Option<DateTime<Utc>>) -> String {
    timestamp
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|| "-".to_string())
}

fn inline_text(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(180).collect()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
