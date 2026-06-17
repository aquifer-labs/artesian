// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

use crate::{
    Distance, MemoryError, MemoryResult, VectorMemoryConfig, PINNED_FASTEMBED_DIMENSIONS,
    PINNED_FASTEMBED_MODEL,
};

pub const OKF_VERSION: &str = "1";
pub const COMPAT_POINT_ID: &str = "__artesian_compat";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionCompat {
    pub artesian_version: String,
    pub okf_version: String,
    pub embedding_model: String,
    pub dimensions: usize,
    pub distance: Distance,
}

impl CollectionCompat {
    pub fn current() -> Self {
        Self {
            artesian_version: env!("CARGO_PKG_VERSION").to_string(),
            okf_version: OKF_VERSION.to_string(),
            embedding_model: PINNED_FASTEMBED_MODEL.to_string(),
            dimensions: PINNED_FASTEMBED_DIMENSIONS,
            distance: Distance::Cosine,
        }
    }

    pub fn from_config(config: &VectorMemoryConfig) -> Self {
        Self {
            artesian_version: env!("CARGO_PKG_VERSION").to_string(),
            okf_version: OKF_VERSION.to_string(),
            embedding_model: config.embedding_model.clone(),
            dimensions: config.dimensions,
            distance: config.distance,
        }
    }

    pub fn validate_compatible(&self, expected: &Self) -> MemoryResult<()> {
        if self.embedding_model != expected.embedding_model
            || self.dimensions != expected.dimensions
            || self.distance != expected.distance
        {
            return Err(MemoryError::CompatMismatch {
                collection_model: self.embedding_model.clone(),
                collection_dimensions: self.dimensions,
                configured_model: expected.embedding_model.clone(),
                configured_dimensions: expected.dimensions,
            });
        }
        Ok(())
    }
}

impl Default for CollectionCompat {
    fn default() -> Self {
        Self::current()
    }
}
