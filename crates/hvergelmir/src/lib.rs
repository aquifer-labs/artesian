// SPDX-License-Identifier: Apache-2.0

//! Optional Hvergelmir sandbox runtime seam.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProfile {
    pub enabled: bool,
    pub image: Option<String>,
    pub allow_network: bool,
    pub mounted_paths: Vec<String>,
}

impl Default for SandboxProfile {
    fn default() -> Self {
        Self {
            enabled: false,
            image: None,
            allow_network: false,
            mounted_paths: Vec::new(),
        }
    }
}
