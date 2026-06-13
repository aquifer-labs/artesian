// SPDX-License-Identifier: Apache-2.0

//! Bifröst TUI crate placeholder.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiStatus {
    pub mode: String,
    pub backend: String,
}

impl TuiStatus {
    pub fn memory_files() -> Self {
        Self {
            mode: "memory".to_string(),
            backend: "files".to_string(),
        }
    }
}
