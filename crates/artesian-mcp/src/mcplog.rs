// SPDX-License-Identifier: Apache-2.0
//! Best-effort plain-text disconnect evidence for `artesian-mcp`.
//!
//! When a Claude Code session silently loses its `mcp__<server>__*` tools mid-session (see
//! `docs/mcp-troubleshooting.md`), the server process itself is usually fine — the client just
//! stopped calling it. This log gives the operator something to check on the server side: did
//! the process start, with which config, and did it exit cleanly (stdin EOF) or with a transport
//! error. Logging is always best-effort: a failure to open or write the log file must never stop
//! the server from starting or serving.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) struct McpLog {
    path: PathBuf,
}

impl McpLog {
    /// Resolve the effective log path and prepare the file. `explicit` is the already-merged
    /// `--log-file` / `ARTESIAN_MCP_LOG` value (the flag wins per clap's own precedence when both
    /// are set); when neither is set, default to `~/.artesian/logs/mcp.log` but only if
    /// `~/.artesian` already exists (so a plain `artesian-mcp` run on a machine that never ran
    /// `artesian init` does not start creating directories under `$HOME`).
    ///
    /// Never fails startup: any I/O error here is reported to stderr once and logging becomes a
    /// no-op for the rest of the process.
    pub(crate) fn init(explicit: Option<PathBuf>) -> Option<Self> {
        let path = explicit.or_else(default_log_path)?;
        match Self::open(&path) {
            Ok(log) => Some(log),
            Err(error) => {
                eprintln!(
                    "warning: artesian-mcp could not open --log-file {}: {error}",
                    path.display()
                );
                None
            }
        }
    }

    fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        rotate_if_oversized(path)?;
        // Touch the file now so a broken path/permission surfaces immediately, not on first log.
        OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn startup(&self, version: &str, pid: u32, config_path: Option<&Path>) {
        let config = config_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".to_string());
        self.append(&format!(
            "startup version={version} pid={pid} config={config}"
        ));
    }

    pub(crate) fn shutdown_clean(&self) {
        self.append("shutdown reason=stdin-eof");
    }

    pub(crate) fn transport_error(&self, error: &dyn std::fmt::Display) {
        self.append(&format!("error transport/serve failed: {error}"));
    }

    fn append(&self, message: &str) {
        let line = format!("{} {message}\n", chrono::Utc::now().to_rfc3339());
        // Best-effort: a write failure here (e.g. disk full, file removed underneath us) must
        // never propagate — logging is not part of the server's contract with its client.
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn default_log_path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    default_log_path_under(&home)
}

/// Split out from `default_log_path` so tests can supply a temp `home` instead of racing on the
/// real, process-global `HOME` environment variable.
fn default_log_path_under(home: &Path) -> Option<PathBuf> {
    let artesian_home = home.join(".artesian");
    artesian_home
        .is_dir()
        .then(|| artesian_home.join("logs").join("mcp.log"))
}

/// Single rotation: if the file already exceeds 5 MB at startup, move it to `<path>.1`
/// (overwriting any previous `.1`), then start fresh. Best-effort — a failure here just means we
/// keep appending to the oversized file instead of failing startup.
fn rotate_if_oversized(path: &Path) -> anyhow::Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= MAX_LOG_BYTES {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    fs::rename(path, PathBuf::from(rotated))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "artesian-mcp-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn startup_writes_a_line_with_version_pid_and_config() {
        let dir = tempdir();
        let path = dir.join("mcp.log");
        let log = McpLog::open(&path).expect("open should succeed");
        log.startup("0.5.9", 4242, Some(Path::new("/tmp/artesian.toml")));
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("startup version=0.5.9 pid=4242 config=/tmp/artesian.toml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shutdown_clean_and_transport_error_are_appended() {
        let dir = tempdir();
        let path = dir.join("mcp.log");
        let log = McpLog::open(&path).expect("open should succeed");
        log.shutdown_clean();
        log.transport_error(&anyhow::anyhow!("boom"));
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("shutdown reason=stdin-eof"));
        assert!(contents.contains("error transport/serve failed: boom"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_file_is_rotated_once_at_startup() {
        let dir = tempdir();
        let path = dir.join("mcp.log");
        {
            // Pre-size a sparse file past the 5 MB cap.
            let file = fs::File::create(&path).unwrap();
            file.set_len(MAX_LOG_BYTES + 1024).unwrap();
        }
        let _log = McpLog::open(&path).expect("open should succeed");
        let rotated = dir.join("mcp.log.1");
        assert!(
            rotated.exists(),
            "expected {} to exist after rotation",
            rotated.display()
        );
        assert!(
            fs::metadata(&path).unwrap().len() < MAX_LOG_BYTES,
            "fresh log file should not carry over the oversized content"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_log_path_present_only_when_artesian_home_dir_exists() {
        let dir = tempdir();
        assert_eq!(default_log_path_under(&dir), None, "no ~/.artesian yet");

        let artesian_home = dir.join(".artesian");
        fs::create_dir_all(&artesian_home).unwrap();
        assert_eq!(
            default_log_path_under(&dir),
            Some(artesian_home.join("logs").join("mcp.log"))
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
