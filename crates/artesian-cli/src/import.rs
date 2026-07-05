// SPDX-License-Identifier: Apache-2.0

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use aquifer::{
    collect_memory_paths, parse_harness_candidates, parse_memory_path, stable_memory_id,
    BackfillFailure, FilesBackend, HarnessKind, MemoryBackend, MemoryQuery, MemoryScope,
    StoreMemory,
};
use headgate::{
    CcsSchema, CommittedContextState, CommittedEntry, DefaultQualifyGate, HeadgateConfig,
    QualifyGate, RecallItem,
};
use headrace::{FilesTaskStore, VectorTaskStore};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    pub scanned: usize,
    pub memory_imported: usize,
    pub memory_skipped_duplicates: usize,
    pub task_imported: usize,
    pub task_skipped_duplicates: usize,
    pub failed: Vec<BackfillFailure>,
    pub index_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub directory: PathBuf,
    pub headwater_root: PathBuf,
    pub user_id: Option<String>,
    pub project: Option<String>,
    /// Emit per-file progress to stderr (stdout stays reserved for the machine-readable summary).
    pub progress: bool,
}

#[derive(Debug, Clone)]
pub struct HarnessImportOptions {
    pub harness: HarnessKind,
    pub path: PathBuf,
    pub project: String,
    pub user_id: Option<String>,
    pub gate: HeadgateConfig,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessImportReport {
    pub harness: String,
    pub project: String,
    pub scanned: usize,
    pub candidates: usize,
    pub admitted: usize,
    pub rejected: usize,
    pub imported: usize,
    pub skipped_duplicates: usize,
    pub failed: Vec<BackfillFailure>,
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    kind: CatalogKind,
    path: String,
    title: String,
    chunks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogKind {
    Memory,
    Task,
}

impl CatalogKind {
    const fn heading(self) -> &'static str {
        match self {
            Self::Memory => "Memory",
            Self::Task => "Tasks",
        }
    }
}

pub async fn import_directory(
    options: ImportOptions,
    primary_memory: Arc<dyn MemoryBackend>,
    write_headwater_copy: bool,
    task_store: &VectorTaskStore,
) -> Result<ImportReport> {
    let mut paths = collect_memory_paths(&options.directory)?;
    paths.sort();

    let headwater_memory =
        write_headwater_copy.then(|| Arc::new(FilesBackend::new(&options.headwater_root)));
    let mut report = ImportReport::default();
    let mut catalog = Vec::new();

    let total = paths.len();
    if options.progress {
        eprintln!(
            "importing {total} file(s) from {}",
            options.directory.display()
        );
    }

    for (idx, path) in paths.iter().enumerate() {
        report.scanned += 1;
        let before_imported = report.memory_imported + report.task_imported;
        let before_dups = report.memory_skipped_duplicates + report.task_skipped_duplicates;
        let before_failed = report.failed.len();
        let is_task = FilesTaskStore::is_task_like_path(path);
        if is_task {
            import_task_path(
                &options.directory,
                path,
                task_store,
                &mut report,
                &mut catalog,
            )
            .await;
        } else {
            import_memory_path(
                &options,
                path,
                primary_memory.as_ref(),
                headwater_memory
                    .as_deref()
                    .map(|backend| backend as &dyn MemoryBackend),
                &mut report,
                &mut catalog,
            )
            .await;
        }
        if options.progress {
            let imported = (report.memory_imported + report.task_imported) - before_imported;
            let dups =
                (report.memory_skipped_duplicates + report.task_skipped_duplicates) - before_dups;
            let outcome = if report.failed.len() > before_failed {
                let reason = report
                    .failed
                    .last()
                    .map(|failure| failure.reason.as_str())
                    .unwrap_or("error");
                format!("FAILED: {}", reason.chars().take(100).collect::<String>())
            } else if is_task {
                if imported > 0 {
                    "task imported".to_string()
                } else {
                    "task (duplicate)".to_string()
                }
            } else if imported > 0 && dups > 0 {
                format!("{imported} imported, {dups} duplicate")
            } else if imported > 0 {
                format!("{imported} imported")
            } else if dups > 0 {
                format!("{dups} duplicate")
            } else {
                "no records".to_string()
            };
            eprintln!(
                "[{}/{}] {} — {}",
                idx + 1,
                total,
                catalog_path(&options.directory, path),
                outcome
            );
        }
    }

    if !catalog.is_empty() {
        report.index_path = Some(write_index(&options.headwater_root, &catalog)?);
    }

    Ok(report)
}

pub async fn import_harness(
    options: HarnessImportOptions,
    backend: &dyn MemoryBackend,
) -> Result<HarnessImportReport> {
    let parsed = parse_harness_candidates(
        options.harness,
        &options.path,
        options.project.clone(),
        options.user_id.as_deref(),
    )?;
    let gate = DefaultQualifyGate::new(options.gate.min_score, options.gate.redundancy_threshold);
    let mut committed =
        CommittedContextState::new(CcsSchema::default(), options.gate.budget_tokens);
    let mut report = HarnessImportReport {
        harness: options.harness.source().to_string(),
        project: options.project.clone(),
        scanned: parsed.scanned,
        candidates: parsed.candidates.len(),
        ..HarnessImportReport::default()
    };

    for candidate in parsed.candidates {
        let mut memory = candidate.memory;
        let node_id = memory
            .node_id
            .clone()
            .unwrap_or_else(|| stable_memory_id(&memory).to_string());
        let ccs = committed_state_for_candidate(
            backend,
            &committed,
            &memory.content,
            &options.project,
            options.gate.recall_limit,
            options.gate.budget_tokens,
        )
        .await;
        let ccs = match ccs {
            Ok(ccs) => ccs,
            Err(error) => {
                report.failed.push(BackfillFailure {
                    file: options.path.clone(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let item = RecallItem::new(
            node_id.clone(),
            memory.content.clone(),
            candidate.qualify_score,
        )
        .with_source(options.harness.source());
        let decision = gate.qualify(&item, &ccs).await;
        memory
            .metadata
            .insert("qualify_reason".to_string(), decision.reason.clone());
        memory.metadata.insert(
            "qualify_score".to_string(),
            format!("{:.3}", decision.score),
        );
        if let Some(slot) = &decision.slot {
            memory.metadata.insert("ocf_slot".to_string(), slot.clone());
            memory.tags.push(format!("ocf-slot:{slot}"));
        }
        if let Some(audit) = &decision.audit {
            memory.confidence = Some(audit.confidence);
            memory.metadata.insert(
                "qualify_confidence".to_string(),
                format!("{:.3}", audit.confidence),
            );
        }
        if !decision.admitted {
            let id = stable_memory_id(&memory);
            match backend.get_node(id.as_str()).await {
                Ok(Some(_)) => report.skipped_duplicates += 1,
                Ok(None) => report.rejected += 1,
                Err(error) => report.failed.push(BackfillFailure {
                    file: options.path.clone(),
                    reason: format!("[{id}]: {error}"),
                }),
            }
            continue;
        }

        let id = stable_memory_id(&memory);
        match backend.get_node(id.as_str()).await {
            Ok(Some(_)) => {
                report.admitted += 1;
                report.skipped_duplicates += 1;
            }
            Ok(None) => match backend.store(memory).await {
                Ok(record) => {
                    report.admitted += 1;
                    report.imported += 1;
                    let slot = decision.slot.unwrap_or_else(|| "fact".to_string());
                    committed.admit(CommittedEntry::new(
                        record.node_id,
                        slot,
                        record.content,
                        decision.score,
                    ));
                }
                Err(error) => report.failed.push(BackfillFailure {
                    file: options.path.clone(),
                    reason: format!("[{id}]: {error}"),
                }),
            },
            Err(error) => report.failed.push(BackfillFailure {
                file: options.path.clone(),
                reason: format!("[{id}]: {error}"),
            }),
        }
    }

    Ok(report)
}

async fn committed_state_for_candidate(
    backend: &dyn MemoryBackend,
    committed: &CommittedContextState,
    content: &str,
    project: &str,
    recall_limit: usize,
    budget_tokens: usize,
) -> Result<CommittedContextState> {
    let mut ccs = CommittedContextState::new(CcsSchema::default(), budget_tokens);
    for entry in committed.entries() {
        ccs.admit(entry.clone());
    }
    let hits = backend
        .find(
            MemoryQuery::new(content)
                .with_project(project)
                .with_limit(recall_limit.max(1)),
        )
        .await?;
    for hit in hits {
        ccs.admit(CommittedEntry::new(
            hit.record.node_id,
            "fact",
            hit.record.content,
            hit.score,
        ));
    }
    Ok(ccs)
}

async fn import_task_path(
    source_root: &Path,
    path: &Path,
    task_store: &VectorTaskStore,
    report: &mut ImportReport,
    catalog: &mut Vec<CatalogEntry>,
) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            push_failure(report, path, error);
            return;
        }
    };
    let task = match FilesTaskStore::parse_task_like_markdown(path, &text) {
        Ok(task) => task,
        Err(error) => {
            push_failure(report, path, error);
            return;
        }
    };
    match task_store.import_task(task).await {
        Ok(outcome) => {
            if outcome.imported() {
                report.task_imported += 1;
            } else {
                report.task_skipped_duplicates += 1;
            }
            catalog.push(CatalogEntry {
                kind: CatalogKind::Task,
                path: catalog_path(source_root, path),
                title: outcome.task().title.clone(),
                chunks: 1,
            });
        }
        Err(error) => push_failure(report, path, error),
    }
}

/// Number of chunks sent to the backend in a single upsert call during bulk import.
/// 256 points per batch keeps Qdrant gRPC messages well under the 4 MB default limit
/// for the typical 384-dimension float32 embeddings used by Artesian.
const IMPORT_BATCH_SIZE: usize = 256;

async fn import_memory_path(
    options: &ImportOptions,
    path: &Path,
    primary_memory: &dyn MemoryBackend,
    headwater_memory: Option<&dyn MemoryBackend>,
    report: &mut ImportReport,
    catalog: &mut Vec<CatalogEntry>,
) {
    let memories = match parse_memory_path(path) {
        Ok(memories) => memories,
        Err(error) => {
            push_failure(report, path, error);
            return;
        }
    };
    let chunk_count = memories.len();
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.replace(['-', '_'], " "))
        .unwrap_or_else(|| "Imported memory".to_string());

    let memories: Vec<StoreMemory> = memories
        .into_iter()
        .map(|memory| {
            with_user_and_project(
                memory,
                options.user_id.as_deref(),
                options.project.as_deref(),
            )
        })
        .collect();

    // Mirror to headwater backend first (FilesBackend uses default sequential bulk_store).
    if let Some(headwater_memory) = headwater_memory {
        let headwater_result = headwater_memory
            .bulk_store(memories.clone(), IMPORT_BATCH_SIZE)
            .await;
        for (id, reason) in headwater_result.failures {
            report.failed.push(BackfillFailure {
                file: path.to_path_buf(),
                reason: format!("headwater mirror [{id}]: {reason}"),
            });
        }
    }

    // Bulk-store to the primary backend: skips per-chunk existence checks for speed.
    let result = primary_memory.bulk_store(memories, IMPORT_BATCH_SIZE).await;
    report.memory_imported += result.stored;
    report.memory_skipped_duplicates += result.skipped;
    for (id, reason) in result.failures {
        report.failed.push(BackfillFailure {
            file: path.to_path_buf(),
            reason: format!("[{id}]: {reason}"),
        });
    }

    catalog.push(CatalogEntry {
        kind: CatalogKind::Memory,
        path: catalog_path(&options.directory, path),
        title,
        chunks: chunk_count,
    });
}

fn with_user_and_project(
    mut memory: StoreMemory,
    user_id: Option<&str>,
    project: Option<&str>,
) -> StoreMemory {
    if let Some(user_id) = user_id {
        if memory.user_id.is_none() {
            memory.user_id = Some(user_id.to_string());
        }
        if memory.scope.is_none() {
            memory.scope = Some(MemoryScope::Shared);
        }
    }
    if let Some(project) = project {
        memory.project = Some(project.to_string());
    }
    memory
}

fn write_index(root: &Path, catalog: &[CatalogEntry]) -> Result<PathBuf> {
    let memory_dir = root.join("memory");
    fs::create_dir_all(&memory_dir)?;
    let path = memory_dir.join("index.md");
    let mut output = String::from(
        "---\ntype: index\ntitle: Artesian Memory Index\n---\n\n# Artesian Memory Index\n\nRead this catalog first, then drill into the listed headwater records or task files as needed.\n",
    );

    for kind in [CatalogKind::Memory, CatalogKind::Task] {
        let entries = catalog
            .iter()
            .filter(|entry| entry.kind == kind)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            continue;
        }
        output.push_str(&format!("\n## {}\n\n", kind.heading()));
        for entry in entries {
            output.push_str(&format!(
                "- `{}` — {} (chunks: {})\n",
                entry.path, entry.title, entry.chunks
            ));
        }
    }

    fs::write(&path, output)?;
    Ok(path)
}

fn catalog_path(source_root: &Path, path: &Path) -> String {
    path.strip_prefix(source_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn push_failure(report: &mut ImportReport, path: &Path, error: impl std::fmt::Display) {
    report.failed.push(BackfillFailure {
        file: path.to_path_buf(),
        reason: error.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use aquifer::{
        MemoryQuery, MemoryResult, SqliteVecVectorStore, TextEmbedder, VectorMemoryBackend,
        VectorMemoryConfig,
    };
    use artesian_test_support::TempDir;

    #[tokio::test]
    async fn import_directory_stamps_requested_project_for_files_backend() {
        let tempdir = TempDir::new("import-project-files");
        let source = tempdir.join("source");
        write_mixed_project_source(&source);

        let backend: Arc<dyn MemoryBackend> = Arc::new(FilesBackend::new(tempdir.join("files")));
        assert_import_stamps_project(tempdir.path(), source, backend).await;
    }

    #[tokio::test]
    async fn import_directory_stamps_requested_project_for_sqlite_vec_backend() {
        let tempdir = TempDir::new("import-project-sqlite");
        let source = tempdir.join("source");
        write_mixed_project_source(&source);

        let store = SqliteVecVectorStore::in_memory().expect("sqlite-vec should open");
        let backend = VectorMemoryBackend::with_embedder(
            store,
            VectorMemoryConfig {
                collection: "import-project".to_string(),
                dimensions: TEST_DIMENSIONS,
                ..VectorMemoryConfig::new("import-project")
            },
            Arc::new(TestEmbedder),
        )
        .expect("backend should construct");
        assert_import_stamps_project(tempdir.path(), source, Arc::new(backend)).await;
    }

    #[tokio::test]
    async fn import_harness_hermes_preserves_structure_scrubs_and_is_idempotent() {
        let tempdir = TempDir::new("import-harness-hermes");
        let source = tempdir.join("hermes");
        std::fs::create_dir_all(&source).expect("source dir should be created");
        std::fs::write(
            source.join("MEMORY.md"),
            r#"§ Project Rules
- Always route this project through Artesian memory.
- The deployment API token: sk-testsecret1234567890 must stay hidden.
- temporary scratch note

## Procedures
- Run `cargo fmt --all` before handing off implementation work.
"#,
        )
        .expect("hermes memory should be written");

        let backend = FilesBackend::new(tempdir.join("files")).with_track_access(false);
        let options = HarnessImportOptions {
            harness: HarnessKind::Hermes,
            path: source.clone(),
            project: "artesian".to_string(),
            user_id: None,
            gate: HeadgateConfig::default(),
        };
        let report = import_harness(options.clone(), &backend)
            .await
            .expect("harness import should succeed");

        assert_eq!(report.scanned, 1);
        assert_eq!(report.candidates, 4);
        assert_eq!(report.imported, 3);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.skipped_duplicates, 0);
        assert!(report.failed.is_empty());

        let hits = backend
            .find(MemoryQuery::new("").with_project("artesian").with_limit(10))
            .await
            .expect("find should succeed");
        assert_eq!(hits.len(), 3, "{hits:#?}");
        assert!(
            hits.iter()
                .all(|hit| hit.record.source.as_deref() == Some("hermes")),
            "source should be hermes: {hits:#?}"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.record.project.as_deref() == Some("artesian")),
            "project should be stamped: {hits:#?}"
        );
        assert!(
            hits.iter()
                .any(|hit| hit.record.metadata.get("section").map(String::as_str)
                    == Some("Project Rules")
                    && hit
                        .record
                        .relations
                        .iter()
                        .any(|relation| relation.predicate == "member_of")),
            "section grouping relation should be preserved: {hits:#?}"
        );
        assert!(
            hits.iter().any(|hit| {
                hit.record
                    .tags
                    .contains(&"memory-type:procedural".to_string())
                    && hit.record.metadata.get("memory_type").map(String::as_str)
                        == Some("procedural")
            }),
            "procedural memory type should be tagged: {hits:#?}"
        );
        let scrubbed = hits
            .iter()
            .find(|hit| hit.record.content.contains("[REDACTED_SECRET]"))
            .expect("secret-bearing record should be imported and scrubbed");
        assert!(!scrubbed.record.content.contains("sk-testsecret"));
        assert_eq!(
            scrubbed
                .record
                .metadata
                .get("secret_scrubbed")
                .map(String::as_str),
            Some("true")
        );
        assert!(
            hits.iter()
                .all(|hit| !hit.record.content.contains("temporary scratch note")),
            "rejected transient line must not land: {hits:#?}"
        );

        let second = import_harness(options, &backend)
            .await
            .expect("second import should succeed");
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped_duplicates, 3);
        let hits_after = backend
            .find(MemoryQuery::new("").with_project("artesian").with_limit(10))
            .await
            .expect("find should succeed");
        assert_eq!(hits_after.len(), 3, "{hits_after:#?}");
    }

    async fn assert_import_stamps_project(
        root: &Path,
        source: PathBuf,
        backend: Arc<dyn MemoryBackend>,
    ) {
        let task_store =
            VectorTaskStore::new(FilesTaskStore::new(root.join("tasks")), backend.clone());
        let report = import_directory(
            ImportOptions {
                directory: source,
                headwater_root: root.join("headwater"),
                user_id: None,
                project: Some("foo".to_string()),
                progress: false,
            },
            backend.clone(),
            false,
            &task_store,
        )
        .await
        .expect("import should succeed");
        let hits = backend
            .find(
                MemoryQuery::new("project stamp sentinel")
                    .with_project("foo")
                    .with_limit(10),
            )
            .await
            .expect("find should succeed");

        assert_eq!(report.scanned, 3);
        assert_eq!(report.memory_imported, 3);
        assert!(report.failed.is_empty());
        assert_eq!(hits.len(), 3, "{hits:#?}");
        assert!(
            hits.iter()
                .all(|hit| hit.record.project.as_deref() == Some("foo")),
            "every imported record should be stamped with foo: {hits:#?}"
        );
    }

    fn write_mixed_project_source(source: &Path) {
        std::fs::create_dir_all(source).expect("source dir should be created");
        std::fs::write(
            source.join("raw.md"),
            "# Raw\n\nraw project stamp sentinel from markdown",
        )
        .expect("raw memory should be written");
        std::fs::write(
            source.join("structured.md"),
            r#"---
type: memory
timestamp: "2026-01-03T00:00:00Z"
node_id: node:structured-project-stamp
tier: l2-scenario
tags:
  - homelab
  - imported
  - memory
---

structured project stamp sentinel from an OCF record
"#,
        )
        .expect("structured memory should be written");
        std::fs::write(
            source.join("structured-stale.md"),
            r#"---
type: memory
timestamp: "2026-01-04T00:00:00Z"
node_id: node:structured-stale-project-stamp
tier: l2-scenario
tags:
  - homelab
  - imported
  - memory
project: stale-project
---

structured project stamp sentinel from a stale-project OCF record
"#,
        )
        .expect("stale-project structured memory should be written");
    }

    const TEST_DIMENSIONS: usize = 8;

    struct TestEmbedder;

    impl TextEmbedder for TestEmbedder {
        fn embed_query(&self, text: &str) -> MemoryResult<Vec<f32>> {
            Ok(test_embedding(text))
        }

        fn embed_passage(&self, text: &str) -> MemoryResult<Vec<f32>> {
            Ok(test_embedding(text))
        }
    }

    fn test_embedding(text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; TEST_DIMENSIONS];
        for token in text.split_whitespace() {
            let index = token.bytes().fold(0usize, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(byte as usize)
            }) % TEST_DIMENSIONS;
            vector[index] += 1.0;
        }
        let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if magnitude > 0.0 {
            for value in &mut vector {
                *value /= magnitude;
            }
        }
        vector
    }
}
