// SPDX-License-Identifier: Apache-2.0

use std::{collections::VecDeque, str::FromStr};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Master,
    Worker,
    Judge,
}

impl Role {
    pub const fn canonical_alias(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Worker => "worker",
            Self::Judge => "judge",
        }
    }

    pub const fn norse_alias(self) -> &'static str {
        match self {
            Self::Master => "odin",
            Self::Worker => "thor",
            Self::Judge => "tyr",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Master => "Óðinn",
            Self::Worker => "Þórr",
            Self::Judge => "Týr",
        }
    }

    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Master => &["master", "odin"],
            Self::Worker => &["worker", "thor"],
            Self::Judge => &["judge", "tyr"],
        }
    }
}

impl FromStr for Role {
    type Err = RoleParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "master" | "odin" => Ok(Self::Master),
            "worker" | "thor" => Ok(Self::Worker),
            "judge" | "tyr" => Ok(Self::Judge),
            other => Err(RoleParseError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown role alias: {value}")]
pub struct RoleParseError {
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErindiStatus {
    Todo,
    Doing,
    Done,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Erindi {
    pub id: String,
    pub title: String,
    pub role: Role,
    pub status: ErindiStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Thing {
    entries: VecDeque<Erindi>,
}

impl Thing {
    pub fn push(&mut self, erindi: Erindi) {
        self.entries.push_back(erindi);
    }

    pub fn pop_next(&mut self) -> Option<Erindi> {
        self.entries.pop_front()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Galdr {
    pub erindi: Erindi,
    pub commit: Option<String>,
    pub completed_at: DateTime<Utc>,
}
