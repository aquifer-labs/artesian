// SPDX-License-Identifier: Apache-2.0

use std::sync::Mutex;

#[cfg(feature = "vector")]
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::{MemoryError, MemoryResult, SearchHit};

pub trait Reranker: Send + Sync {
    fn rerank(
        &self,
        query: &str,
        hits: Vec<SearchHit>,
        limit: usize,
    ) -> MemoryResult<Vec<SearchHit>>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalLexicalReranker;

impl Reranker for LocalLexicalReranker {
    fn rerank(
        &self,
        query: &str,
        mut hits: Vec<SearchHit>,
        limit: usize,
    ) -> MemoryResult<Vec<SearchHit>> {
        let terms = query_terms(query);
        hits.sort_by(|left, right| {
            let left_score = rerank_score(&left.record.content, &terms, left.score);
            let right_score = rerank_score(&right.record.content, &terms, right.score);
            right_score
                .partial_cmp(&left_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.record.node_id.cmp(&right.record.node_id))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}

/// Reranks hits with the pinned fastembed cross-encoder, opening the ONNX session on first use.
///
/// Prefer [`FastembedReranker::shared`] over [`FastembedReranker::new`]: the reranker model is
/// larger than the embedder, and one session per `MemoryServer` means one copy of the weights per
/// HTTP session.
#[cfg(feature = "vector")]
pub struct FastembedReranker {
    inner: Mutex<Option<TextRerank>>,
    batch_size: usize,
}

#[cfg(feature = "vector")]
impl FastembedReranker {
    /// Build a reranker with no session loaded yet.
    ///
    /// Note that a missing or unreadable model surfaces on the first rerank call rather than here,
    /// because the session is opened lazily.
    pub fn new() -> MemoryResult<Self> {
        Ok(Self {
            inner: Mutex::new(None),
            batch_size: 8,
        })
    }

    /// The process-wide reranker, loading the model at most once per process.
    pub fn shared() -> std::sync::Arc<Self> {
        static SHARED: std::sync::OnceLock<std::sync::Arc<FastembedReranker>> =
            std::sync::OnceLock::new();
        SHARED
            .get_or_init(|| {
                std::sync::Arc::new(Self {
                    inner: Mutex::new(None),
                    batch_size: 8,
                })
            })
            .clone()
    }

    fn open_model() -> MemoryResult<TextRerank> {
        TextRerank::try_new(
            RerankInitOptions::new(RerankerModel::BGERerankerV2M3)
                .with_show_download_progress(false),
        )
        .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))
    }
}

#[cfg(feature = "vector")]
impl Reranker for FastembedReranker {
    fn rerank(
        &self,
        query: &str,
        hits: Vec<SearchHit>,
        limit: usize,
    ) -> MemoryResult<Vec<SearchHit>> {
        if hits.len() <= limit {
            return Ok(hits);
        }
        let documents = hits
            .iter()
            .map(|hit| hit.record.content.as_str())
            .collect::<Vec<_>>();
        let mut slot = crate::vector_memory::lock_recovering(&self.inner);
        if slot.is_none() {
            *slot = Some(Self::open_model()?);
        }
        let reranker = slot
            .as_mut()
            .expect("session is loaded by the branch above");
        let ranked = reranker
            .rerank(query, documents, false, Some(self.batch_size))
            .map_err(|error| MemoryError::BackendUnavailable(error.to_string()))?;
        let mut output = Vec::with_capacity(limit);
        for result in ranked.into_iter().take(limit) {
            if let Some(hit) = hits.get(result.index) {
                let mut hit = hit.clone();
                hit.score = result.score;
                output.push(hit);
            }
        }
        Ok(output)
    }
}

pub fn query_terms(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .filter(|term| !term.is_empty())
        .collect()
}

fn rerank_score(content: &str, terms: &[String], base_score: f32) -> f32 {
    let content = content.to_ascii_lowercase();
    let lexical = terms
        .iter()
        .map(|term| content.matches(term).count() as f32)
        .sum::<f32>();
    lexical + base_score * 0.01
}

#[cfg(test)]
mod tests {
    use crate::{MemoryId, MemoryRecord, MemoryTier, SearchHit};

    use super::{LocalLexicalReranker, Reranker};

    #[test]
    fn lexical_reranker_promotes_exact_query_terms() {
        let hits = vec![
            SearchHit::keyword(record("node:b", "generic memory"), 100.0),
            SearchHit::keyword(record("node:a", "qdrant endpoint preflight"), 1.0),
        ];

        let reranked = LocalLexicalReranker
            .rerank("qdrant preflight", hits, 1)
            .expect("rerank should succeed");

        assert_eq!(reranked.len(), 1);
        assert_eq!(reranked[0].record.node_id, "node:a");
    }

    fn record(node_id: &str, content: &str) -> MemoryRecord {
        MemoryRecord::new(
            MemoryId::new(format!("id:{node_id}")),
            node_id,
            content,
            Vec::new(),
            Default::default(),
            MemoryTier::L1Atom,
        )
    }
}
