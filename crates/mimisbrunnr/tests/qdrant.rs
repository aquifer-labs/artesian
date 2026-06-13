// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "qdrant")]

use mimisbrunnr::QdrantBackendConfig;

#[test]
fn qdrant_backend_pins_fastembed_model_and_dimensions() {
    let config = QdrantBackendConfig::new("http://localhost:6334", "general");

    assert_eq!(config.embedding_model, "intfloat/multilingual-e5-small");
    assert_eq!(config.dimensions, 384);
}
