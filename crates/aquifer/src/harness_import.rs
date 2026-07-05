// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{MemoryError, MemoryResult, MemoryScope, MemoryTier, Relation, StoreMemory};

const OCF_HARNESS_SCHEMA: &str = "ocf.harness-memory";
const OCF_HARNESS_SCHEMA_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessKind {
    Hermes,
    ClaudeCode,
    Codex,
}

impl HarnessKind {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Hermes => "hermes",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessMemoryCandidate {
    pub memory: StoreMemory,
    pub qualify_score: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessParseReport {
    pub scanned: usize,
    pub candidates: Vec<HarnessMemoryCandidate>,
}

pub fn parse_harness_candidates(
    harness: HarnessKind,
    path: impl AsRef<Path>,
    project: impl Into<String>,
    user_id: Option<&str>,
) -> MemoryResult<HarnessParseReport> {
    let root = path.as_ref();
    let project = project.into();
    let sources = collect_harness_sources(harness, root)?;
    let mut report = HarnessParseReport {
        scanned: sources.len(),
        candidates: Vec::new(),
    };

    for source in sources {
        let text = std::fs::read_to_string(&source.path)?;
        let facts = match source.kind {
            SourceKind::Markdown => parse_sectioned_markdown(&text),
            SourceKind::CodexSession => parse_codex_session_text(&text),
        };
        let relative_path = relative_display(root, &source.path);
        report
            .candidates
            .extend(facts.into_iter().filter_map(|fact| {
                candidate_from_fact(harness, &relative_path, fact, &project, user_id)
            }));
    }

    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Markdown,
    CodexSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HarnessSource {
    path: PathBuf,
    kind: SourceKind,
}

fn collect_harness_sources(harness: HarnessKind, root: &Path) -> MemoryResult<Vec<HarnessSource>> {
    if !root.exists() {
        return Err(MemoryError::InvalidFile(format!(
            "harness path does not exist: {}",
            root.display()
        )));
    }
    if root.is_file() {
        return Ok(vec![HarnessSource {
            path: root.to_path_buf(),
            kind: source_kind_for_file(harness, root),
        }]);
    }

    let mut paths = match harness {
        HarnessKind::Hermes => collect_named_markdown(root, &["MEMORY.md", "USER.md"])?,
        HarnessKind::ClaudeCode => collect_claude_code_markdown(root)?,
        HarnessKind::Codex => collect_codex_sources(root)?,
    };
    paths.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(paths)
}

fn source_kind_for_file(harness: HarnessKind, path: &Path) -> SourceKind {
    if harness == HarnessKind::Codex
        && path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("jsonl")
        })
    {
        SourceKind::CodexSession
    } else {
        SourceKind::Markdown
    }
}

fn collect_named_markdown(root: &Path, names: &[&str]) -> MemoryResult<Vec<HarnessSource>> {
    let mut paths = Vec::new();
    collect_files(
        root,
        &mut |path| {
            let is_match = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    names
                        .iter()
                        .any(|candidate| name.eq_ignore_ascii_case(candidate))
                });
            is_match.then_some(SourceKind::Markdown)
        },
        &mut paths,
    )?;
    Ok(paths)
}

fn collect_claude_code_markdown(root: &Path) -> MemoryResult<Vec<HarnessSource>> {
    let mut paths = Vec::new();
    collect_files(
        root,
        &mut |path| {
            let is_memory_file = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("MEMORY.md"));
            let is_memory_dir_note = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("memory"));
            (is_memory_file || is_memory_dir_note).then_some(SourceKind::Markdown)
        },
        &mut paths,
    )?;
    Ok(paths)
}

fn collect_codex_sources(root: &Path) -> MemoryResult<Vec<HarnessSource>> {
    let memories = if root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("memories"))
    {
        root.to_path_buf()
    } else {
        root.join("memories")
    };
    if memories.exists() {
        return collect_codex_markdown(&memories);
    }

    let sessions = if root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("sessions"))
    {
        root.to_path_buf()
    } else {
        root.join("sessions")
    };
    if sessions.exists() {
        return collect_codex_sessions(&sessions);
    }

    Ok(Vec::new())
}

fn collect_codex_markdown(root: &Path) -> MemoryResult<Vec<HarnessSource>> {
    let mut paths = Vec::new();
    collect_files(
        root,
        &mut |path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                .then_some(SourceKind::Markdown)
        },
        &mut paths,
    )?;
    Ok(paths)
}

fn collect_codex_sessions(root: &Path) -> MemoryResult<Vec<HarnessSource>> {
    let mut paths = Vec::new();
    collect_files(
        root,
        &mut |path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| {
                    let is_session = ["json", "jsonl", "md", "txt"]
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate));
                    if !is_session {
                        return None;
                    }
                    if extension.eq_ignore_ascii_case("json")
                        || extension.eq_ignore_ascii_case("jsonl")
                    {
                        Some(SourceKind::CodexSession)
                    } else {
                        Some(SourceKind::Markdown)
                    }
                })
        },
        &mut paths,
    )?;
    Ok(paths)
}

fn collect_files(
    directory: &Path,
    accept: &mut dyn FnMut(&Path) -> Option<SourceKind>,
    paths: &mut Vec<HarnessSource>,
) -> MemoryResult<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if !is_skippable_harness_dir(&path) {
                collect_files(&path, accept, paths)?;
            }
        } else if let Some(kind) = accept(&path) {
            paths.push(HarnessSource { path, kind });
        }
    }
    Ok(())
}

fn is_skippable_harness_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".fastembed_cache"
                    | ".venv"
                    | "node_modules"
                    | "target"
                    | "__pycache__"
                    | "venv"
                    | "dist"
                    | "build"
                    | "vendor"
            )
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedFact {
    section: String,
    line: usize,
    text: String,
}

fn parse_sectioned_markdown(text: &str) -> Vec<ParsedFact> {
    let mut section = "root".to_string();
    let mut section_line = 1usize;
    let mut paragraph = Vec::new();
    let mut paragraph_line = 1usize;
    let mut facts = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if let Some(heading) = section_heading(line) {
            flush_paragraph(
                &mut facts,
                &section,
                paragraph_line,
                std::mem::take(&mut paragraph),
            );
            section = heading;
            section_line = line_number;
            continue;
        }

        if line.trim().is_empty() {
            flush_paragraph(
                &mut facts,
                &section,
                paragraph_line,
                std::mem::take(&mut paragraph),
            );
            continue;
        }

        if let Some(item) = strip_list_marker(line) {
            flush_paragraph(
                &mut facts,
                &section,
                paragraph_line,
                std::mem::take(&mut paragraph),
            );
            push_fact(&mut facts, &section, line_number, item);
        } else {
            if paragraph.is_empty() {
                paragraph_line = line_number.max(section_line);
            }
            paragraph.push(line.trim().to_string());
        }
    }

    flush_paragraph(&mut facts, &section, paragraph_line, paragraph);
    facts
}

fn parse_codex_session_text(text: &str) -> Vec<ParsedFact> {
    let mut facts = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let mut strings = Vec::new();
            collect_session_strings(&value, &mut strings);
            for text in strings {
                for fact in split_atomic_text(&text) {
                    push_fact(&mut facts, "session", line_number, fact);
                }
            }
        } else {
            for fact in split_atomic_text(trimmed) {
                push_fact(&mut facts, "session", line_number, fact);
            }
        }
    }
    facts
}

fn collect_session_strings(value: &serde_json::Value, output: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                output.push(trimmed.to_string());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_session_strings(value, output);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if is_session_text_key(key)
                    || matches!(
                        value,
                        serde_json::Value::Array(_) | serde_json::Value::Object(_)
                    )
                {
                    collect_session_strings(value, output);
                }
            }
        }
        _ => {}
    }
}

fn is_session_text_key(key: &str) -> bool {
    matches!(
        key,
        "content" | "text" | "summary" | "message" | "result" | "reasoning" | "output"
    )
}

fn section_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('§') {
        let heading = rest.trim_matches(|character: char| {
            character.is_whitespace() || character == '-' || character == ':'
        });
        return (!heading.is_empty()).then(|| heading.to_string());
    }
    let without_markers = trimmed.trim_start_matches('#');
    (without_markers.len() < trimmed.len() && without_markers.starts_with(' '))
        .then(|| without_markers.trim().to_string())
        .filter(|heading| !heading.is_empty())
}

fn strip_list_marker(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ ", "- [ ] ", "- [x] ", "* [ ] ", "* [x] "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }

    let (number, rest) = trimmed.split_once(". ")?;
    (!number.is_empty() && number.chars().all(|character| character.is_ascii_digit()))
        .then_some(rest.trim())
}

fn flush_paragraph(
    facts: &mut Vec<ParsedFact>,
    section: &str,
    line: usize,
    paragraph: Vec<String>,
) {
    let text = paragraph.join(" ");
    for fact in split_atomic_text(&text) {
        push_fact(facts, section, line, fact);
    }
}

fn split_atomic_text(text: &str) -> Vec<&str> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= 280 {
        return vec![text];
    }

    text.split(". ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn push_fact(facts: &mut Vec<ParsedFact>, section: &str, line: usize, text: &str) {
    let text = normalize_fact_text(text);
    if !text.is_empty() {
        facts.push(ParsedFact {
            section: section.to_string(),
            line,
            text,
        });
    }
}

fn normalize_fact_text(text: &str) -> String {
    text.trim()
        .trim_matches(|character: char| character == '-' || character == '*')
        .trim()
        .to_string()
}

fn candidate_from_fact(
    harness: HarnessKind,
    relative_path: &str,
    fact: ParsedFact,
    project: &str,
    user_id: Option<&str>,
) -> Option<HarnessMemoryCandidate> {
    let (content, scrubbed) = scrub_secrets(&fact.text);
    let content = content.trim();
    if content.is_empty() {
        return None;
    }
    let memory_type = classify_memory_type(content, &fact.section);
    let source = harness.source();
    let group_id = section_group_id(source, relative_path, &fact.section);
    let node_id = harness_node_id(source, relative_path, &fact.section, content);
    let mut metadata = BTreeMap::from([
        ("headwater_type".to_string(), "memory".to_string()),
        ("ocf_schema".to_string(), OCF_HARNESS_SCHEMA.to_string()),
        (
            "ocf_schema_version".to_string(),
            OCF_HARNESS_SCHEMA_VERSION.to_string(),
        ),
        ("harness".to_string(), source.to_string()),
        ("source_path".to_string(), relative_path.to_string()),
        ("section".to_string(), fact.section.clone()),
        ("section_group".to_string(), group_id.clone()),
        ("source_line".to_string(), fact.line.to_string()),
        ("memory_type".to_string(), memory_type.to_string()),
        (
            "provenance".to_string(),
            format!("{source}:{relative_path}:{}", fact.line),
        ),
    ]);
    if scrubbed {
        metadata.insert("secret_scrubbed".to_string(), "true".to_string());
    }

    let tags = vec![
        "harness-import".to_string(),
        "ocf".to_string(),
        format!("source:{source}"),
        format!("memory-type:{memory_type}"),
    ];
    let relations = vec![
        Relation::new(
            node_id.clone(),
            "member_of",
            group_id.clone(),
            node_id.clone(),
        ),
        Relation::new(
            group_id.clone(),
            "section_title",
            fact.section,
            node_id.clone(),
        ),
        Relation::new(group_id, "source_file", relative_path, node_id.clone()),
    ];
    let qualify_score = durable_fact_score(content);

    Some(HarnessMemoryCandidate {
        memory: StoreMemory {
            content: content.to_string(),
            tags,
            metadata,
            tier: MemoryTier::L1Atom,
            node_id: Some(node_id),
            created_at: None,
            scope: user_id.map(|_| MemoryScope::Shared),
            agent_id: None,
            session_id: None,
            task_id: None,
            user_id: user_id.map(str::to_string),
            project: Some(project.to_string()),
            source: Some(source.to_string()),
            author_id: Some(source.to_string()),
            confidence: None,
            relations,
        },
        qualify_score,
    })
}

fn section_group_id(source: &str, relative_path: &str, section: &str) -> String {
    format!(
        "harness:{source}:section:{}:{}",
        slug(section),
        short_hash(&format!("{relative_path}\n{section}"))
    )
}

fn harness_node_id(source: &str, relative_path: &str, section: &str, content: &str) -> String {
    format!(
        "harness:{source}:{}",
        short_hash(&format!("{relative_path}\n{section}\n{content}"))
    )
}

fn slug(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for character in value.to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_dash = false;
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "root".to_string()
    } else {
        slug.chars().take(48).collect()
    }
}

fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
        .chars()
        .take(16)
        .collect()
}

fn relative_display(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        _ => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| path.display().to_string()),
    }
}

fn classify_memory_type(content: &str, section: &str) -> &'static str {
    let lower = format!("{} {}", section, content).to_ascii_lowercase();
    if contains_any(
        &lower,
        &[
            "procedure",
            "workflow",
            "playbook",
            "command",
            "steps",
            "run ",
            "execute ",
            "check ",
            "verify ",
            "use `",
        ],
    ) {
        return "procedural";
    }
    if contains_any(
        &lower,
        &[
            "incident",
            "postmortem",
            "session",
            "yesterday",
            "today",
            "last time",
            "shipped",
            "fixed",
            "debugged",
            "completed",
        ],
    ) || contains_year(&lower)
    {
        return "episodic";
    }
    "semantic"
}

fn durable_fact_score(content: &str) -> f32 {
    let lower = content.to_ascii_lowercase();
    let word_count = lower.split_whitespace().count();
    if word_count < 3 || contains_any(&lower, &["scratch", "temporary", "delete me", "ignore"]) {
        return 0.0;
    }
    if contains_any(
        &lower,
        &[
            "buy milk",
            "lunch",
            "random note",
            "note to self",
            "todo maybe",
            "draft thought",
        ],
    ) {
        return 0.0;
    }
    if contains_any(
        &lower,
        &[
            "always",
            "never",
            "must",
            "should",
            "requires",
            "require",
            "prefer",
            "decision",
            "source of truth",
            "project",
            "operator",
            "configuration",
            "config",
            "policy",
            "workflow",
            "run ",
            "use ",
            "route",
            "store",
            "import",
            "memory",
        ],
    ) {
        return 1.0;
    }
    if word_count >= 5 {
        0.8
    } else {
        0.0
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn contains_year(text: &str) -> bool {
    text.split(|character: char| !character.is_ascii_digit())
        .any(|token| {
            token.len() == 4
                && token
                    .parse::<u16>()
                    .is_ok_and(|year| (2020..=2100).contains(&year))
        })
}

fn scrub_secrets(text: &str) -> (String, bool) {
    let mut scrubbed = false;
    let lines = text
        .lines()
        .map(|line| {
            let (line, changed) = scrub_secret_line(line);
            scrubbed |= changed;
            line
        })
        .collect::<Vec<_>>();
    (lines.join("\n"), scrubbed)
}

fn scrub_secret_line(line: &str) -> (String, bool) {
    let lower = line.to_ascii_lowercase();
    if let Some(output) = scrub_bearer_token(line, &lower) {
        return (output, true);
    }
    if is_secret_assignment(&lower) {
        if let Some(index) = line.find('=').or_else(|| line.find(':')) {
            let prefix = line[..=index].trim_end();
            return (format!("{prefix} [REDACTED_SECRET]"), true);
        }
    }

    let mut changed = false;
    let tokens = line
        .split_whitespace()
        .map(|token| {
            let (scrubbed, token_changed) = scrub_token(token, is_secret_context(&lower));
            changed |= token_changed;
            scrubbed
        })
        .collect::<Vec<_>>();
    (tokens.join(" "), changed)
}

fn scrub_bearer_token(line: &str, lower: &str) -> Option<String> {
    let index = lower.find("bearer ")?;
    let prefix_end = index + "bearer".len();
    Some(format!("{} [REDACTED_SECRET]", &line[..prefix_end]))
}

fn is_secret_assignment(lower: &str) -> bool {
    is_secret_context(lower) && (lower.contains('=') || lower.contains(':'))
}

fn is_secret_context(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            "password",
            "passwd",
            "api_key",
            "api key",
            "apikey",
            "secret",
            "token",
            "credential",
            "private key",
        ],
    )
}

fn scrub_token(token: &str, secret_context: bool) -> (String, bool) {
    let without_leading = token.trim_start_matches(is_token_punctuation);
    let leading = token.len() - without_leading.len();
    let core = without_leading.trim_end_matches(is_token_punctuation);
    let core_end = leading + core.len();
    let prefix = &token[..leading];
    let suffix = &token[core_end..];
    if core.is_empty() {
        return (token.to_string(), false);
    }
    if is_known_secret_token(core) || (secret_context && is_high_entropy_token(core)) {
        (format!("{prefix}[REDACTED_SECRET]{suffix}"), true)
    } else {
        (token.to_string(), false)
    }
}

fn is_token_punctuation(character: char) -> bool {
    matches!(
        character,
        '"' | '\'' | '`' | ',' | '.' | ';' | ')' | '(' | '[' | ']' | '{' | '}'
    )
}

fn is_known_secret_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    (lower.starts_with("sk-") && token.len() >= 12)
        || (lower.starts_with("ghp_") && token.len() >= 12)
        || (lower.starts_with("gho_") && token.len() >= 12)
        || (lower.starts_with("github_pat_") && token.len() >= 20)
        || (lower.starts_with("xoxb-") && token.len() >= 12)
        || (lower.starts_with("xoxp-") && token.len() >= 12)
        || (token.starts_with("AKIA") && token.len() >= 16)
        || looks_like_jwt(token)
}

fn looks_like_jwt(token: &str) -> bool {
    let parts = token.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| part.len() >= 10 && part.chars().all(is_base64_url_char))
}

fn is_base64_url_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn is_high_entropy_token(token: &str) -> bool {
    token.len() >= 16
        && token
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stable_memory_id;

    #[test]
    fn hermes_parser_preserves_sections_and_scrubs_secrets() {
        let temp = std::env::temp_dir().join(format!(
            "artesian-harness-{}-{}",
            std::process::id(),
            short_hash("parser")
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(
            temp.join("MEMORY.md"),
            "§ Project Rules\n- Always route this project through Artesian memory.\n- API token: sk-testsecret1234567890 must stay hidden.\n",
        )
        .unwrap();

        let report =
            parse_harness_candidates(HarnessKind::Hermes, &temp, "artesian", None).unwrap();
        assert_eq!(report.scanned, 1);
        assert_eq!(report.candidates.len(), 2);
        assert!(report.candidates.iter().any(|candidate| {
            candidate
                .memory
                .metadata
                .get("section")
                .is_some_and(|section| section == "Project Rules")
        }));
        let secret = report
            .candidates
            .iter()
            .find(|candidate| candidate.memory.content.contains("[REDACTED_SECRET]"))
            .expect("secret should be redacted");
        assert!(!secret.memory.content.contains("sk-testsecret"));
        assert_eq!(
            secret
                .memory
                .metadata
                .get("secret_scrubbed")
                .map(String::as_str),
            Some("true")
        );

        let first = stable_memory_id(&report.candidates[0].memory);
        let second_report =
            parse_harness_candidates(HarnessKind::Hermes, &temp, "artesian", None).unwrap();
        assert_eq!(first, stable_memory_id(&second_report.candidates[0].memory));
        let _ = std::fs::remove_dir_all(&temp);
    }
}
