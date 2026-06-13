// SPDX-License-Identifier: Apache-2.0

use std::{path::Path, path::PathBuf, process::Command};

#[test]
fn cli_memory_mode_round_trip_and_spawn_alias_work() {
    let tempdir = TempDir::new("cli");
    let binary = env!("CARGO_BIN_EXE_brunnr");

    let init = Command::new(binary)
        .arg("init")
        .current_dir(tempdir.path())
        .output()
        .expect("init should run");
    assert!(init.status.success(), "{}", stderr(&init));

    let spawn = Command::new(binary)
        .args(["spawn", "thor", "codex"])
        .current_dir(tempdir.path())
        .output()
        .expect("spawn should run");
    assert!(spawn.status.success(), "{}", stderr(&spawn));
    assert!(stdout(&spawn).contains("role=worker alias=thor agent=codex"));

    let store = Command::new(binary)
        .args([
            "memory",
            "store",
            "Brunnr memory mode works",
            "--tag",
            "smoke",
            "--node-id",
            "node:cli",
        ])
        .current_dir(tempdir.path())
        .output()
        .expect("store should run");
    assert!(store.status.success(), "{}", stderr(&store));

    let find = Command::new(binary)
        .args(["memory", "find", "works", "--node-id", "node:cli"])
        .current_dir(tempdir.path())
        .output()
        .expect("find should run");
    assert!(find.status.success(), "{}", stderr(&find));
    assert!(stdout(&find).contains("node:cli\tBrunnr memory mode works"));
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "brunnr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
