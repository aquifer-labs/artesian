// SPDX-License-Identifier: Apache-2.0

use std::process::Command;

use brunnr_test_support::TempDir;

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
