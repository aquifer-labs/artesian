// SPDX-License-Identifier: Apache-2.0

use rmcp::handler::server::wrapper::Parameters;

use brunnr_mcp::{FindRequest, MemoryServer, StoreRequest};
use brunnr_test_support::TempDir;

#[tokio::test]
async fn memory_tools_store_and_find_with_files_backend() {
    let tempdir = TempDir::new("mcp");
    let server = MemoryServer::new(tempdir.path());

    let stored = server
        .memory_store(Parameters(StoreRequest {
            content: "MCP memory tool round trip".to_string(),
            tags: Some(vec!["mcp".to_string()]),
            node_id: Some("node:mcp".to_string()),
        }))
        .await
        .expect("store should succeed")
        .0;

    let found = server
        .memory_find(Parameters(FindRequest {
            query: "round".to_string(),
            limit: Some(5),
            node_id: Some("node:mcp".to_string()),
        }))
        .await
        .expect("find should succeed")
        .0;

    assert_eq!(stored.node_id, "node:mcp");
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].node_id, "node:mcp");
    assert_eq!(found.hits[0].content, "MCP memory tool round trip");
}
