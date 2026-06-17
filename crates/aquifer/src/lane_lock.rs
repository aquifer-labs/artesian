// SPDX-License-Identifier: Apache-2.0

use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::time;

use crate::{MemoryError, MemoryResult};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLaneLock {
    root: PathBuf,
    timeout: Duration,
}

impl SessionLaneLock {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn default_rooted() -> Self {
        Self::new(
            std::env::var_os("ARTESIAN_LANE_LOCK_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".artesian").join("locks")),
        )
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn acquire(
        &self,
        collection: &str,
        session_id: Option<&str>,
    ) -> MemoryResult<SessionLaneGuard> {
        let lane = lane_name(collection, session_id);
        let path = self.root.join(format!("{lane}.lock"));
        fs::create_dir_all(&self.root)?;
        let started = Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    serde_json::to_writer_pretty(
                        file,
                        &LaneLockFile {
                            version: 1,
                            owner_pid: std::process::id(),
                            lane: lane.clone(),
                            created_at_unix_ms: now_unix_ms(),
                        },
                    )?;
                    return Ok(SessionLaneGuard { path, active: true });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    remove_stale_lock(&path)?;
                    if started.elapsed() >= self.timeout {
                        return Err(MemoryError::LaneLockTimeout {
                            lane,
                            timeout_millis: self.timeout.as_millis(),
                        });
                    }
                    time::sleep(POLL_INTERVAL).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

#[derive(Debug)]
pub struct SessionLaneGuard {
    path: PathBuf,
    active: bool,
}

impl SessionLaneGuard {
    pub fn release(mut self) -> MemoryResult<()> {
        self.release_inner()?;
        Ok(())
    }

    fn release_inner(&mut self) -> std::io::Result<()> {
        if !self.active {
            return Ok(());
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for SessionLaneGuard {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LaneLockFile {
    version: u32,
    owner_pid: u32,
    lane: String,
    created_at_unix_ms: u128,
}

fn lane_name(collection: &str, session_id: Option<&str>) -> String {
    let session = session_id.unwrap_or("shared");
    format!(
        "{}__{}",
        sanitize_lane_part(collection),
        sanitize_lane_part(session)
    )
}

fn sanitize_lane_part(input: &str) -> String {
    let output = input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if output.is_empty() {
        "default".to_string()
    } else {
        output
    }
}

fn remove_stale_lock(path: &Path) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Ok(lock) = serde_json::from_str::<LaneLockFile>(&text) else {
        return Ok(());
    };
    if !process_alive(lock.owner_pid) {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_alive(pid: u32) -> bool {
    pid == std::process::id()
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}
