// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use crate::{RrfOptions, SearchHit, SearchSource};

pub fn reciprocal_rank_fusion(channels: &[Vec<SearchHit>], options: RrfOptions) -> Vec<SearchHit> {
    let mut merged: BTreeMap<String, (SearchHit, f32)> = BTreeMap::new();

    for channel in channels {
        for (rank, hit) in channel.iter().enumerate() {
            let score = 1.0 / (options.rank_constant + rank as f32 + 1.0);
            let key = hit.record.node_id.clone();
            merged
                .entry(key)
                .and_modify(|(_, existing_score)| *existing_score += score)
                .or_insert_with(|| (hit.clone(), score));
        }
    }

    let mut hits = merged
        .into_values()
        .map(|(mut hit, score)| {
            hit.score = score;
            hit.source = SearchSource::Hybrid;
            hit
        })
        .collect::<Vec<_>>();

    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.record.node_id.cmp(&right.record.node_id))
    });
    hits.truncate(options.limit);
    hits
}

#[cfg(test)]
#[path = "rrf_tests.rs"]
mod tests;
