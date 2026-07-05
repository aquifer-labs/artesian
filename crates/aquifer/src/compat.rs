// SPDX-License-Identifier: Apache-2.0

use std::sync::Once;

use serde::{Deserialize, Serialize};

use crate::{
    Distance, MemoryError, MemoryResult, VectorMemoryConfig, PINNED_FASTEMBED_DIMENSIONS,
    PINNED_FASTEMBED_MODEL,
};

pub const HEADWATER_VERSION: &str = "1";
#[deprecated(note = "renamed to HEADWATER_VERSION")]
pub const OKF_VERSION: &str = HEADWATER_VERSION;
pub const COMPAT_POINT_ID: &str = "__artesian_compat";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollectionCompat {
    pub artesian_version: String,
    pub headwater_version: String,
    pub embedding_model: String,
    pub dimensions: usize,
    pub distance: Distance,
}

#[derive(Debug, Deserialize)]
struct CollectionCompatWire {
    artesian_version: String,
    #[serde(default)]
    headwater_version: Option<String>,
    #[serde(default)]
    okf_version: Option<String>,
    embedding_model: String,
    dimensions: usize,
    distance: Distance,
}

impl<'de> Deserialize<'de> for CollectionCompat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionCompatWire::deserialize(deserializer)?;
        if wire.okf_version.is_some() {
            warn_legacy_okf_version_once();
        }
        Ok(Self {
            artesian_version: wire.artesian_version,
            headwater_version: wire
                .headwater_version
                .or(wire.okf_version)
                .unwrap_or_else(|| HEADWATER_VERSION.to_string()),
            embedding_model: wire.embedding_model,
            dimensions: wire.dimensions,
            distance: wire.distance,
        })
    }
}

static LEGACY_OKF_VERSION_WARNING: Once = Once::new();

fn warn_legacy_okf_version_once() {
    LEGACY_OKF_VERSION_WARNING.call_once(|| {
        eprintln!("warning: compatibility key okf_version is deprecated; use headwater_version");
    });
}

impl CollectionCompat {
    pub fn current() -> Self {
        Self {
            artesian_version: env!("CARGO_PKG_VERSION").to_string(),
            headwater_version: HEADWATER_VERSION.to_string(),
            embedding_model: PINNED_FASTEMBED_MODEL.to_string(),
            dimensions: PINNED_FASTEMBED_DIMENSIONS,
            distance: Distance::Cosine,
        }
    }

    pub fn from_config(config: &VectorMemoryConfig) -> Self {
        Self {
            artesian_version: env!("CARGO_PKG_VERSION").to_string(),
            headwater_version: HEADWATER_VERSION.to_string(),
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
