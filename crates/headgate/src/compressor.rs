// SPDX-License-Identifier: Apache-2.0

use futures_util::{FutureExt, future::BoxFuture};

use crate::{HeadgateResult, metrics::count_tokens};

/// Compresses committed content to fit the saturation budget — the "cognitive compressor".
///
/// Implementations may be deterministic (extractive, the always-available default) or
/// LLM-backed (an agent CLI such as Codex / Claude Code / Gemini / opencode, or a local
/// server such as Ollama / LM Studio / `mlx_lm.server`). The controller treats them all
/// uniformly: hand over content and a token target, get back content that fits.
pub trait Compressor: Send + Sync {
    /// Compress `content` to at most `target_tokens`. May return the input unchanged if it
    /// already fits.
    fn compress(
        &self,
        content: &str,
        target_tokens: usize,
    ) -> BoxFuture<'_, HeadgateResult<String>>;
}

/// No-op compressor — returns content unchanged. Used when compression is disabled.
#[derive(Debug, Clone, Default)]
pub struct NoopCompressor;

impl Compressor for NoopCompressor {
    fn compress(
        &self,
        content: &str,
        _target_tokens: usize,
    ) -> BoxFuture<'_, HeadgateResult<String>> {
        let content = content.to_string();
        async move { Ok(content) }.boxed()
    }
}

/// Deterministic extractive compressor: keeps leading sentences until the token budget is
/// reached. Zero-dependency, no LLM call — the always-available default so the control
/// plane works fully offline.
#[derive(Debug, Clone, Default)]
pub struct ExtractiveCompressor;

impl Compressor for ExtractiveCompressor {
    fn compress(
        &self,
        content: &str,
        target_tokens: usize,
    ) -> BoxFuture<'_, HeadgateResult<String>> {
        let content = content.to_string();
        async move {
            if count_tokens(&content) <= target_tokens {
                return Ok(content);
            }
            let mut kept = String::new();
            for sentence in split_sentences(&content) {
                let candidate = if kept.is_empty() {
                    sentence.to_string()
                } else {
                    format!("{kept} {sentence}")
                };
                if count_tokens(&candidate) > target_tokens {
                    break;
                }
                kept = candidate;
            }
            if kept.is_empty() {
                // A single oversized sentence: hard-truncate by characters as a last resort.
                Ok(truncate_to_tokens(&content, target_tokens))
            } else {
                Ok(kept)
            }
        }
        .boxed()
    }
}

/// LLM-backed compressor: asks a model to rewrite content to fit the token budget while
/// preserving meaning. Falls back to the deterministic [`ExtractiveCompressor`] if the model
/// is unreachable or returns something that still overflows, so compression never fails the
/// commit-loop.
#[cfg(feature = "llm")]
pub struct LlmCompressor {
    client: std::sync::Arc<dyn crate::LlmClient>,
    fallback: ExtractiveCompressor,
}

#[cfg(feature = "llm")]
impl LlmCompressor {
    pub fn new(client: std::sync::Arc<dyn crate::LlmClient>) -> Self {
        Self {
            client,
            fallback: ExtractiveCompressor,
        }
    }
}

#[cfg(feature = "llm")]
impl Compressor for LlmCompressor {
    fn compress(
        &self,
        content: &str,
        target_tokens: usize,
    ) -> BoxFuture<'_, HeadgateResult<String>> {
        let content = content.to_string();
        async move {
            if count_tokens(&content) <= target_tokens {
                return Ok(content);
            }
            let request = crate::LlmRequest::new(format!(
                "Compress the following note to at most {target_tokens} tokens while preserving \
every concrete fact, decision, identifier, and number. Reply with ONLY the compressed note, no \
preamble.\n\nNote:\n{content}"
            ))
            .with_temperature(0.0)
            .with_max_tokens(target_tokens.saturating_mul(2).max(32));

            match self.client.complete(request).await {
                Ok(text)
                    if count_tokens(text.trim()) <= target_tokens && !text.trim().is_empty() =>
                {
                    Ok(text.trim().to_string())
                }
                // Model unreachable or still over budget: fall back to extractive.
                _ => self.fallback.compress(&content, target_tokens).await,
            }
        }
        .boxed()
    }
}

fn split_sentences(text: &str) -> Vec<&str> {
    text.split_inclusive(['.', '!', '?', '\n'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect()
}

fn truncate_to_tokens(text: &str, target_tokens: usize) -> String {
    if target_tokens == 0 {
        return String::new();
    }
    // Approximate 1 token ~ 4 chars, then tighten until the token count fits.
    let mut end = (target_tokens * 4).min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut slice = text[..end].to_string();
    while count_tokens(&slice) > target_tokens && !slice.is_empty() {
        let mut shorter = slice.len().saturating_sub(8);
        while shorter > 0 && !slice.is_char_boundary(shorter) {
            shorter -= 1;
        }
        slice.truncate(shorter);
    }
    slice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_unchanged_when_within_budget() {
        let compressor = ExtractiveCompressor;
        let content = "short note.";
        let out = compressor.compress(content, 100).await.expect("compress");
        assert_eq!(out, content);
    }

    #[tokio::test]
    async fn keeps_leading_sentences_within_budget() {
        let compressor = ExtractiveCompressor;
        let content = "First sentence here. Second sentence here. Third sentence here. \
                       Fourth sentence here. Fifth sentence here.";
        let target = 12;
        let out = compressor
            .compress(content, target)
            .await
            .expect("compress");
        assert!(
            count_tokens(&out) <= target,
            "compressed fits budget: {out:?}"
        );
        assert!(out.starts_with("First sentence"));
        assert!(count_tokens(&out) < count_tokens(content));
    }

    #[tokio::test]
    async fn truncates_single_oversized_sentence() {
        let compressor = ExtractiveCompressor;
        let content = "a very long run-on clause that keeps going and going and going \
                       without any sentence boundary to split on at all whatsoever";
        let target = 5;
        let out = compressor
            .compress(content, target)
            .await
            .expect("compress");
        assert!(count_tokens(&out) <= target);
        assert!(!out.is_empty());
    }

    #[tokio::test]
    async fn noop_compressor_passes_through() {
        let compressor = NoopCompressor;
        let out = compressor
            .compress("anything at all", 1)
            .await
            .expect("compress");
        assert_eq!(out, "anything at all");
    }
}
