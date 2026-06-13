// SPDX-License-Identifier: Apache-2.0

use super::*;

#[tokio::test]
async fn files_backend_stores_date_tagged_markdown_and_finds_it() {
    let tempdir = TempDir::new("files-store");
    let backend = FilesBackend::new(tempdir.path());

    let stored = backend
        .store(StoreMemory {
            content: "Files backend keeps memory readable".to_string(),
            tags: vec!["files".to_string()],
            metadata: BTreeMap::new(),
            tier: MemoryTier::L1Atom,
            node_id: Some("node:files".to_string()),
        })
        .await
        .expect("store should succeed");

    let date_tag = stored.created_at.format("%Y-%m-%d").to_string();
    let path = backend.record_path(&date_tag, &stored.id);
    let rendered = fs::read_to_string(path)
        .await
        .expect("record should be readable");
    let hits = backend
        .find(MemoryQuery::new("readable"))
        .await
        .expect("find should succeed");

    assert!(rendered.contains(&format!("[{date_tag}] Files backend keeps memory readable")));
    assert_eq!(hits, vec![SearchHit::keyword(stored, 1.0)]);
}

#[tokio::test]
async fn files_backend_drills_down_by_node_id() {
    let tempdir = TempDir::new("files-node");
    let backend = FilesBackend::new(tempdir.path());
    let stored = backend
        .store(StoreMemory {
            content: "Ground truth evidence".to_string(),
            tags: Vec::new(),
            metadata: BTreeMap::new(),
            tier: MemoryTier::L0Raw,
            node_id: Some("node:evidence".to_string()),
        })
        .await
        .expect("store should succeed");

    assert_eq!(
        backend
            .get_node("node:evidence")
            .await
            .expect("get_node should succeed"),
        Some(stored)
    );
}

struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "brunnr-{name}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&path).expect("temp dir should be created");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
