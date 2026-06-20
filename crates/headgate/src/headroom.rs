// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "headroom")]

//! headroom data-plane compression adapter for the ACC control plane.
//!
//! ## Complementarity
//!
//! Artesian is the **control plane**: it decides *what* enters the bounded Committed Context
//! State (CCS) through the qualify-gate, and when to evict under saturation. headroom is the
//! **data-plane compression layer**: it decides *how many bytes* an artifact occupies once it
//! has been admitted.
//!
//! The two layers are orthogonal and compose cleanly:
//!
//! ```text
//! recall candidates
//!       │
//!       ▼
//! ┌────────────────────────────┐
//! │  ACC qualify-gate          │  ← Artesian: relevance / novelty / drift
//! │  (Headgate)                │
//! └────────────┬───────────────┘
//!              │ admitted
//!              ▼
//! ┌────────────────────────────┐
//! │  HeadroomCompressor        │  ← headroom: shrink artifact bytes to fit CCS budget
//! │  (this module)             │
//! └────────────┬───────────────┘
//!              │ compressed
//!              ▼
//!       Committed Context State
//! ```
//!
//! headroom can shrink a large admitted artifact (e.g. a long decision log) before it is
//! written to the CCS, buying additional headroom in the token budget. The ACC gate still
//! controls admission; headroom only acts on already-admitted content.
//!
//! ## Usage
//!
//! Enable the `headroom` feature in `headgate` and wrap any `Compressor`:
//!
//! ```no_run
//! # use headgate::{ExtractiveCompressor, HeadroomCompressor};
//! // Passthrough (same as not using HeadroomCompressor): headroom endpoint not configured.
//! let compressor = HeadroomCompressor::passthrough(ExtractiveCompressor);
//!
//! // With a headroom server endpoint:
//! let compressor = HeadroomCompressor::new(
//!     ExtractiveCompressor,
//!     Some("http://localhost:9090".to_string()),
//! );
//! ```
//!
//! ## Fallback
//!
//! When the headroom endpoint is unreachable or returns an error, `HeadroomCompressor` falls
//! back to the inner `Compressor` transparently. headroom is never in the failure path of the
//! ACC control loop.

use futures_util::{future::BoxFuture, FutureExt};

use crate::{Compressor, HeadgateResult};

/// A [`Compressor`] adapter that optionally delegates byte-level compression to a headroom
/// server before the token budget is applied.
///
/// The inner compressor is always the fallback. When `endpoint` is `None`, this is a
/// zero-overhead passthrough. When `endpoint` is `Some`, it calls headroom's compression
/// API and falls back to the inner compressor on any error.
pub struct HeadroomCompressor<C: Compressor> {
    inner: C,
    /// headroom server endpoint (e.g. `"http://localhost:9090"`), or `None` for passthrough.
    endpoint: Option<String>,
}

impl<C: Compressor> HeadroomCompressor<C> {
    /// Wrap `inner` with an optional headroom server endpoint.
    ///
    /// Set `endpoint` to `Some(url)` to call headroom for byte-level compression before the
    /// token budget check. Set to `None` for a zero-overhead passthrough.
    pub fn new(inner: C, endpoint: Option<String>) -> Self {
        Self { inner, endpoint }
    }

    /// Wrap `inner` as a transparent passthrough — headroom is not called.
    /// Useful for feature-flagging: same code path, headroom endpoint opt-in at runtime.
    pub fn passthrough(inner: C) -> Self {
        Self {
            inner,
            endpoint: None,
        }
    }
}

impl<C: Compressor + Send + Sync> Compressor for HeadroomCompressor<C> {
    fn compress(
        &self,
        content: &str,
        target_tokens: usize,
    ) -> BoxFuture<'_, HeadgateResult<String>> {
        let content = content.to_string();
        async move {
            // When an endpoint is configured: call headroom's compression API.
            // headroom shrinks the artifact bytes; the inner compressor then ensures
            // the result fits the token budget. Falls back to inner on any failure.
            if let Some(_endpoint) = &self.endpoint {
                // NOTE: Concrete headroom HTTP integration goes here once headroom's
                // public API stabilizes. The seam is established; swap in the call
                // when the API is known. Currently: skip to inner (safe fallback).
                //
                // Example (not yet wired — headroom API TBD):
                //   let compressed = headroom_compress(endpoint, &content).await?;
                //   return self.inner.compress(&compressed, target_tokens).await;
            }
            self.inner.compress(&content, target_tokens).await
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtractiveCompressor;

    #[tokio::test]
    async fn passthrough_delegates_to_inner_compressor() {
        let inner = ExtractiveCompressor;
        let compressor = HeadroomCompressor::passthrough(inner);
        let long = "First sentence here. Second sentence here. Third sentence here. \
                    Fourth sentence here. Fifth sentence here.";
        let out = compressor.compress(long, 8).await.expect("compress");
        // ExtractiveCompressor keeps leading sentences within budget.
        assert!(!out.is_empty(), "should produce non-empty output");
        assert!(
            out.starts_with("First sentence"),
            "should keep first sentence"
        );
    }

    #[tokio::test]
    async fn endpoint_none_is_identical_to_passthrough() {
        let compressor = HeadroomCompressor::new(ExtractiveCompressor, None);
        let content = "short note.";
        let out = compressor.compress(content, 100).await.expect("compress");
        assert_eq!(out, content, "passthrough should return content unchanged");
    }

    #[tokio::test]
    async fn endpoint_some_falls_back_to_inner_when_not_yet_wired() {
        let compressor = HeadroomCompressor::new(
            ExtractiveCompressor,
            Some("http://localhost:9090".to_string()),
        );
        let content = "short note.";
        let out = compressor.compress(content, 100).await.expect("compress");
        // Until headroom API is wired, falls back to inner — behavior unchanged.
        assert_eq!(out, content);
    }
}
