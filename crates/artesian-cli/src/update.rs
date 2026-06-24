// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};
use artesian_process_agent::ProcessSupervisor;

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{kill, Signal},
    unistd::Pid,
};

const HOMEBREW_FORMULA: &str = "aquifer-labs/tap/artesian";
const RESTART_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn update(restart_stale: bool) -> Result<()> {
    println!("artesian update");

    let brew_outcome = run_brew_update();
    print!("{}", render_brew_update_outcome(&brew_outcome));

    let report = collect_update_surface_report();
    print!("{}", render_update_surface_report(&report));

    let restart_targets = stale_mcp_restart_targets(&report.running_mcp.processes);
    if restart_stale {
        let restart_outcome = restart_stale_mcp_processes(&restart_targets);
        print!("{}", render_restart_outcome(&restart_outcome));
    } else if !restart_targets.is_empty() {
        println!(
            "Restart: run `artesian update --restart-stale` to terminate only stale artesian-mcp servers, or restart those MCP clients manually."
        );
    }

    println!(
        "Doctor: run `artesian doctor` after MCP clients restart to verify config/backend health."
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrewUpgradePlan {
    SkipUnavailable,
    SkipUnmanaged,
    Upgrade,
}

fn brew_upgrade_plan(brew_available: bool, brew_managed: bool) -> BrewUpgradePlan {
    if !brew_available {
        BrewUpgradePlan::SkipUnavailable
    } else if !brew_managed {
        BrewUpgradePlan::SkipUnmanaged
    } else {
        BrewUpgradePlan::Upgrade
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrewUpdateOutcome {
    Unavailable,
    NotManaged { current_exe: Option<PathBuf> },
    UpgradeSucceeded,
    UpgradeNonZero,
    UpgradeFailed(String),
}

fn run_brew_update() -> BrewUpdateOutcome {
    let brew_available = command_success("brew", ["--version"]);
    let brew_managed = brew_available && command_success("brew", ["list", HOMEBREW_FORMULA]);

    match brew_upgrade_plan(brew_available, brew_managed) {
        BrewUpgradePlan::SkipUnavailable => BrewUpdateOutcome::Unavailable,
        BrewUpgradePlan::SkipUnmanaged => BrewUpdateOutcome::NotManaged {
            current_exe: env::current_exe().ok(),
        },
        BrewUpgradePlan::Upgrade => {
            println!("Homebrew: upgrading {HOMEBREW_FORMULA}...");
            match Command::new("brew")
                .args(["upgrade", HOMEBREW_FORMULA])
                .status()
            {
                Ok(status) if status.success() => BrewUpdateOutcome::UpgradeSucceeded,
                Ok(_) => BrewUpdateOutcome::UpgradeNonZero,
                Err(error) => BrewUpdateOutcome::UpgradeFailed(error.to_string()),
            }
        }
    }
}

fn command_success<const N: usize>(program: &str, args: [&str; N]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn render_brew_update_outcome(outcome: &BrewUpdateOutcome) -> String {
    match outcome {
        BrewUpdateOutcome::Unavailable => {
            "Homebrew: not found; skipped the brew upgrade and will still inspect installed surfaces.\n  install with `brew install aquifer-labs/tap/artesian`, or update with your source/homelab install method.\n\n".to_string()
        }
        BrewUpdateOutcome::NotManaged { current_exe } => {
            let exe = current_exe
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            format!(
                "Homebrew: present, but {HOMEBREW_FORMULA} is not installed; skipped brew upgrade.\n  current process binary: {exe}\n  install with `brew install {HOMEBREW_FORMULA}`, or update with your manual install method.\n\n"
            )
        }
        BrewUpdateOutcome::UpgradeSucceeded => {
            "Homebrew: upgrade completed.\n\n".to_string()
        }
        BrewUpdateOutcome::UpgradeNonZero => {
            "Homebrew: `brew upgrade` returned non-zero (often means already up to date); continuing with surface detection.\n\n".to_string()
        }
        BrewUpdateOutcome::UpgradeFailed(error) => {
            format!("Homebrew: upgrade failed to start/run ({error}); continuing with surface detection.\n\n")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdateSurfaceReport {
    binaries: Vec<InstalledBinary>,
    registrations: Vec<McpRegistrationReport>,
    zed_extension: ZedExtensionStatus,
    running_mcp: RunningMcpReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstalledBinary {
    name: String,
    path: Option<PathBuf>,
    resolved_path: Option<PathBuf>,
    version: VersionProbe,
}

impl InstalledBinary {
    fn comparison_path(&self) -> Option<PathBuf> {
        self.resolved_path.clone().or_else(|| self.path.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionProbe {
    NotInstalled,
    Found(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpRegistrationReport {
    surface: String,
    registered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ZedExtensionStatus {
    Present(Vec<PathBuf>),
    Absent { searched: Vec<PathBuf> },
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunningMcpReport {
    processes: Vec<McpProcessStatus>,
    discovery_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunningMcpProcess {
    pid: u32,
    pgid: Option<i32>,
    exe: Option<PathBuf>,
    command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpBinaryState {
    Current,
    Stale,
    Unknown,
}

impl McpBinaryState {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "STALE",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpProcessStatus {
    process: RunningMcpProcess,
    binary_state: McpBinaryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RestartTarget {
    pid: u32,
    pgid: Option<i32>,
    mode: RestartMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RestartMode {
    ProcessGroup(i32),
    ProcessOnly,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RestartOutcome {
    terminated: Vec<RestartTarget>,
    failed: Vec<(RestartTarget, String)>,
}

fn collect_update_surface_report() -> UpdateSurfaceReport {
    let binaries = ["artesian", "artesian-mcp"]
        .into_iter()
        .map(detect_installed_binary)
        .collect::<Vec<_>>();
    let current_mcp_binary = binaries
        .iter()
        .find(|binary| binary.name == "artesian-mcp")
        .and_then(InstalledBinary::comparison_path);
    let registrations = crate::mcp_registration_status()
        .into_iter()
        .map(|(surface, registered)| McpRegistrationReport {
            surface: surface.to_string(),
            registered,
        })
        .collect();
    let zed_extension = detect_zed_extension();
    let running_mcp = match discover_running_mcp_processes() {
        Ok(processes) => RunningMcpReport {
            processes: classify_running_mcp_processes(processes, current_mcp_binary.as_deref()),
            discovery_error: None,
        },
        Err(error) => RunningMcpReport {
            processes: Vec::new(),
            discovery_error: Some(error.to_string()),
        },
    };

    UpdateSurfaceReport {
        binaries,
        registrations,
        zed_extension,
        running_mcp,
    }
}

fn detect_installed_binary(name: &str) -> InstalledBinary {
    let path = resolve_on_path(name);
    let resolved_path = path.as_deref().and_then(|path| fs::canonicalize(path).ok());
    let version = path
        .as_deref()
        .map(probe_binary_version)
        .unwrap_or(VersionProbe::NotInstalled);
    InstalledBinary {
        name: name.to_string(),
        path,
        resolved_path,
        version,
    }
}

fn resolve_on_path(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(command))
            .find(|candidate| candidate.is_file())
    })
}

fn probe_binary_version(path: &Path) -> VersionProbe {
    match Command::new(path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let first_line = text.lines().next().unwrap_or_default().trim();
            if first_line.is_empty() {
                VersionProbe::Failed("version output was empty".to_string())
            } else {
                VersionProbe::Found(first_line.to_string())
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let message = stderr
                .lines()
                .chain(stdout.lines())
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("version command failed");
            VersionProbe::Failed(short_message(message))
        }
        Err(error) => VersionProbe::Failed(error.to_string()),
    }
}

fn short_message(message: &str) -> String {
    const MAX: usize = 160;
    let trimmed = message.trim();
    let mut chars = trimmed.chars();
    let shortened = chars.by_ref().take(MAX).collect::<String>();
    if chars.next().is_none() {
        trimmed.to_string()
    } else {
        format!("{shortened}...")
    }
}

fn detect_zed_extension() -> ZedExtensionStatus {
    match crate::home_dir() {
        Ok(home) => detect_zed_extension_under_home(&home),
        Err(error) => ZedExtensionStatus::Unknown(error.to_string()),
    }
}

fn detect_zed_extension_under_home(home: &Path) -> ZedExtensionStatus {
    let searched = zed_extension_roots(home);
    let mut matches = Vec::new();
    for root in &searched {
        matches.extend(find_artesian_dirs(root, 2));
    }
    matches.sort();
    matches.dedup();
    if matches.is_empty() {
        ZedExtensionStatus::Absent { searched }
    } else {
        ZedExtensionStatus::Present(matches)
    }
}

fn zed_extension_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join("Library")
            .join("Application Support")
            .join("Zed")
            .join("extensions"),
        home.join(".local")
            .join("share")
            .join("zed")
            .join("extensions"),
        home.join(".config").join("zed").join("extensions"),
        home.join(".config").join("Zed").join("extensions"),
    ];
    roots.sort();
    roots.dedup();
    roots
}

fn find_artesian_dirs(root: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(read_dir) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let mentions_artesian = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase().contains("artesian"))
            .unwrap_or(false);
        if mentions_artesian {
            matches.push(path.clone());
        }
        if depth > 0 {
            matches.extend(find_artesian_dirs(&path, depth - 1));
        }
    }
    matches
}

fn classify_running_mcp_processes(
    processes: Vec<RunningMcpProcess>,
    current_mcp_binary: Option<&Path>,
) -> Vec<McpProcessStatus> {
    let current_mcp_binary = current_mcp_binary.map(normalize_comparison_path);
    processes
        .into_iter()
        .map(|process| {
            let binary_state = match (process.exe.as_deref(), current_mcp_binary.as_ref()) {
                (Some(exe), Some(current)) => {
                    if normalize_comparison_path(exe) == *current {
                        McpBinaryState::Current
                    } else {
                        McpBinaryState::Stale
                    }
                }
                _ => McpBinaryState::Unknown,
            };
            McpProcessStatus {
                process,
                binary_state,
            }
        })
        .collect()
}

fn normalize_comparison_path(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    let text = text.strip_suffix(" (deleted)").unwrap_or(&text);
    let path = PathBuf::from(text);
    fs::canonicalize(&path).unwrap_or(path)
}

fn stale_mcp_restart_targets(processes: &[McpProcessStatus]) -> Vec<RestartTarget> {
    let stale = processes
        .iter()
        .filter(|process| process.binary_state == McpBinaryState::Stale)
        .collect::<Vec<_>>();
    let stale_group_leaders = stale
        .iter()
        .filter_map(|process| {
            let pgid = process.process.pgid?;
            (pgid > 0 && pgid as u32 == process.process.pid).then_some(pgid)
        })
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for process in stale {
        let mode = match process.process.pgid {
            Some(pgid) if stale_group_leaders.contains(&pgid) => RestartMode::ProcessGroup(pgid),
            _ => RestartMode::ProcessOnly,
        };
        let dedupe_key = match mode {
            RestartMode::ProcessGroup(pgid) => RestartTargetKey::ProcessGroup(pgid),
            RestartMode::ProcessOnly => RestartTargetKey::Process(process.process.pid),
        };
        if seen.insert(dedupe_key) {
            targets.push(RestartTarget {
                pid: process.process.pid,
                pgid: process.process.pgid,
                mode,
            });
        }
    }
    targets
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RestartTargetKey {
    ProcessGroup(i32),
    Process(u32),
}

fn restart_stale_mcp_processes(targets: &[RestartTarget]) -> RestartOutcome {
    let mut outcome = RestartOutcome::default();
    for target in targets {
        match terminate_restart_target(target) {
            Ok(()) => outcome.terminated.push(target.clone()),
            Err(error) => outcome.failed.push((target.clone(), error.to_string())),
        }
    }
    outcome
}

#[cfg(unix)]
fn terminate_restart_target(target: &RestartTarget) -> Result<()> {
    match target.mode {
        RestartMode::ProcessGroup(pgid) => ProcessSupervisor::default_for_current_dir()
            .terminate_group(pgid)
            .with_context(|| format!("terminate process group {pgid}")),
        RestartMode::ProcessOnly => terminate_pid(target.pid, RESTART_GRACE)
            .with_context(|| format!("terminate process {}", target.pid)),
    }
}

#[cfg(not(unix))]
fn terminate_restart_target(_target: &RestartTarget) -> Result<()> {
    anyhow::bail!("automatic stale MCP restart is only supported on Unix")
}

#[cfg(unix)]
fn terminate_pid(pid: u32, grace: Duration) -> io::Result<()> {
    if !send_pid_signal(pid, Signal::SIGTERM)? {
        return Ok(());
    }
    std::thread::sleep(grace);
    if pid_alive(pid) {
        let _ = send_pid_signal(pid, Signal::SIGKILL)?;
    }
    Ok(())
}

#[cfg(unix)]
fn send_pid_signal(pid: u32, signal: Signal) -> io::Result<bool> {
    match kill(Pid::from_raw(pid as i32), signal) {
        Ok(()) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(Errno::EPERM) if signal == Signal::SIGKILL => Ok(false),
        Err(error) => Err(io::Error::other(format!(
            "signal {signal:?} to pid {pid}: {error}"
        ))),
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn discover_running_mcp_processes() -> io::Result<Vec<RunningMcpProcess>> {
    let read_dir = match fs::read_dir("/proc") {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut processes = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let root = entry.path();
        let comm = fs::read_to_string(root.join("comm")).unwrap_or_default();
        let argv = read_proc_cmdline(&root.join("cmdline"));
        if !is_artesian_mcp_process(&comm, &argv) {
            continue;
        }
        let command = if argv.is_empty() {
            comm.trim().to_string()
        } else {
            argv.join(" ")
        };
        processes.push(RunningMcpProcess {
            pid,
            pgid: read_linux_pgid(&root.join("stat")).ok(),
            exe: fs::read_link(root.join("exe")).ok(),
            command,
        });
    }
    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

#[cfg(target_os = "linux")]
fn read_proc_cmdline(path: &Path) -> Vec<String> {
    fs::read(path)
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
fn read_linux_pgid(path: &Path) -> io::Result<i32> {
    let stat = fs::read_to_string(path)?;
    let after_comm = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .ok_or_else(|| io::Error::other("malformed /proc stat"))?;
    after_comm
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| io::Error::other("missing process group in /proc stat"))?
        .parse::<i32>()
        .map_err(io::Error::other)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn discover_running_mcp_processes() -> io::Result<Vec<RunningMcpProcess>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,pgid=,comm=,args="])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("ps failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ps_mcp_processes(&text))
}

#[cfg(not(unix))]
fn discover_running_mcp_processes() -> io::Result<Vec<RunningMcpProcess>> {
    Ok(Vec::new())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn parse_ps_mcp_processes(text: &str) -> Vec<RunningMcpProcess> {
    let mut processes = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        let pgid = fields.next().and_then(|field| field.parse::<i32>().ok());
        let comm = fields.next().unwrap_or_default();
        let args = fields.collect::<Vec<_>>().join(" ");
        let argv = args
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if !is_artesian_mcp_process(comm, &argv) {
            continue;
        }
        let argv0 = argv.first().map(String::as_str).unwrap_or(comm);
        let exe = if comm.contains('/') {
            Some(PathBuf::from(comm))
        } else if argv0.contains('/') {
            Some(PathBuf::from(argv0))
        } else {
            None
        };
        processes.push(RunningMcpProcess {
            pid,
            pgid,
            exe,
            command: if args.is_empty() {
                comm.to_string()
            } else {
                args.to_string()
            },
        });
    }
    processes.sort_by_key(|process| process.pid);
    processes
}

#[cfg(unix)]
fn is_artesian_mcp_process(comm: &str, argv: &[String]) -> bool {
    path_basename(comm) == Some("artesian-mcp")
        || argv
            .first()
            .and_then(|arg| path_basename(arg))
            .is_some_and(|name| name == "artesian-mcp")
}

#[cfg(unix)]
fn path_basename(path: &str) -> Option<&str> {
    Path::new(path.trim())
        .file_name()
        .and_then(|name| name.to_str())
}

fn render_update_surface_report(report: &UpdateSurfaceReport) -> String {
    let mut output = String::new();
    output.push_str("Installed binaries:\n");
    for binary in &report.binaries {
        match &binary.path {
            Some(path) => {
                output.push_str(&format!("  {}: {}\n", binary.name, path.display()));
                if let Some(resolved) = &binary.resolved_path {
                    output.push_str(&format!("    resolved: {}\n", resolved.display()));
                }
                match &binary.version {
                    VersionProbe::Found(version) => {
                        output.push_str(&format!("    version: {version}\n"));
                    }
                    VersionProbe::Failed(error) => {
                        output.push_str(&format!("    version: unavailable ({error})\n"));
                    }
                    VersionProbe::NotInstalled => {}
                }
            }
            None => {
                output.push_str(&format!("  {}: not found on PATH\n", binary.name));
            }
        }
    }

    output.push_str("\nMCP registrations:\n");
    for registration in &report.registrations {
        output.push_str(&format!(
            "  {}: {}\n",
            registration.surface,
            if registration.registered {
                "registered"
            } else {
                "missing"
            }
        ));
    }

    output.push_str("\nZed extension:\n");
    match &report.zed_extension {
        ZedExtensionStatus::Present(paths) => {
            for path in paths {
                output.push_str(&format!("  artesian-zed detected: {}\n", path.display()));
            }
        }
        ZedExtensionStatus::Absent { searched } => {
            output.push_str("  artesian-zed not detected");
            if !searched.is_empty() {
                output.push_str(" (searched ");
                output.push_str(
                    &searched
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                output.push(')');
            }
            output.push('\n');
        }
        ZedExtensionStatus::Unknown(error) => {
            output.push_str(&format!("  unable to inspect Zed extensions: {error}\n"));
        }
    }

    output.push_str("\nRunning artesian-mcp processes:\n");
    if let Some(error) = &report.running_mcp.discovery_error {
        output.push_str(&format!("  unable to inspect running processes: {error}\n"));
    } else if report.running_mcp.processes.is_empty() {
        output.push_str("  none detected\n");
    } else {
        for process in &report.running_mcp.processes {
            let exe = process
                .process
                .exe
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            let pgid = process
                .process
                .pgid
                .map(|pgid| pgid.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            output.push_str(&format!(
                "  pid={} pgid={} exe={} state={}\n",
                process.process.pid,
                pgid,
                exe,
                process.binary_state.label()
            ));
            if process.binary_state == McpBinaryState::Unknown
                && !process.process.command.is_empty()
            {
                output.push_str("    command observed; executable path could not be verified\n");
            }
        }
    }

    let stale_count = report
        .running_mcp
        .processes
        .iter()
        .filter(|process| process.binary_state == McpBinaryState::Stale)
        .count();
    let unknown_count = report
        .running_mcp
        .processes
        .iter()
        .filter(|process| process.binary_state == McpBinaryState::Unknown)
        .count();
    if stale_count > 0 {
        output.push_str(&format!(
            "\nWARNING: {stale_count} running artesian-mcp process(es) are still on an old binary. Agents keep using those old MCP servers until they are restarted.\n"
        ));
    }
    if unknown_count > 0 {
        output.push_str(&format!(
            "Note: {unknown_count} running artesian-mcp process(es) could not be matched to an executable path; restart them manually if their memory tools still report the old version.\n"
        ));
    }
    output.push('\n');
    output
}

fn render_restart_outcome(outcome: &RestartOutcome) -> String {
    if outcome.terminated.is_empty() && outcome.failed.is_empty() {
        return "Restart: no stale artesian-mcp processes needed termination.\n".to_string();
    }

    let mut output = String::from("Restart:\n");
    for target in &outcome.terminated {
        match target.mode {
            RestartMode::ProcessGroup(pgid) => output.push_str(&format!(
                "  terminated stale artesian-mcp process group pgid={} (pid={})\n",
                pgid, target.pid
            )),
            RestartMode::ProcessOnly => output.push_str(&format!(
                "  terminated stale artesian-mcp pid={} (shared/unknown process group)\n",
                target.pid
            )),
        }
    }
    for (target, error) in &outcome.failed {
        output.push_str(&format!(
            "  failed to terminate stale artesian-mcp pid={} pgid={}: {}\n",
            target.pid,
            target
                .pgid
                .map(|pgid| pgid.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
            error
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_marks_installed_surfaces_and_stale_processes() {
        let current = PathBuf::from("/opt/homebrew/Cellar/artesian/0.5.0/bin/artesian");
        let processes = classify_running_mcp_processes(
            vec![
                RunningMcpProcess {
                    pid: 100,
                    pgid: Some(100),
                    exe: Some(PathBuf::from(
                        "/opt/homebrew/Cellar/artesian/0.4.0/bin/artesian",
                    )),
                    command: "artesian-mcp --config artesian.toml".to_string(),
                },
                RunningMcpProcess {
                    pid: 101,
                    pgid: Some(101),
                    exe: Some(current.clone()),
                    command: "artesian-mcp --config artesian.toml".to_string(),
                },
                RunningMcpProcess {
                    pid: 102,
                    pgid: Some(200),
                    exe: None,
                    command: "artesian-mcp --config artesian.toml".to_string(),
                },
            ],
            Some(&current),
        );
        let report = UpdateSurfaceReport {
            binaries: vec![
                InstalledBinary {
                    name: "artesian".to_string(),
                    path: Some(PathBuf::from("/opt/homebrew/bin/artesian")),
                    resolved_path: Some(current.clone()),
                    version: VersionProbe::Found("artesian 0.5.0".to_string()),
                },
                InstalledBinary {
                    name: "artesian-mcp".to_string(),
                    path: Some(PathBuf::from("/opt/homebrew/bin/artesian-mcp")),
                    resolved_path: Some(current),
                    version: VersionProbe::Found("artesian-mcp 0.5.0".to_string()),
                },
            ],
            registrations: vec![
                McpRegistrationReport {
                    surface: "Claude Code (user ~/.claude.json)".to_string(),
                    registered: true,
                },
                McpRegistrationReport {
                    surface: "Codex".to_string(),
                    registered: true,
                },
                McpRegistrationReport {
                    surface: "Zed".to_string(),
                    registered: false,
                },
            ],
            zed_extension: ZedExtensionStatus::Present(vec![PathBuf::from(
                "/Users/example/.config/zed/extensions/dev/artesian-zed",
            )]),
            running_mcp: RunningMcpReport {
                processes,
                discovery_error: None,
            },
        };

        let text = render_update_surface_report(&report);

        assert!(text.contains("artesian: /opt/homebrew/bin/artesian"));
        assert!(text.contains("version: artesian 0.5.0"));
        assert!(text.contains("Claude Code (user ~/.claude.json): registered"));
        assert!(text.contains("artesian-zed detected"));
        assert!(text.contains("pid=100 pgid=100"));
        assert!(text.contains("state=STALE"));
        assert!(text.contains("pid=101 pgid=101"));
        assert!(text.contains("state=current"));
        assert!(text.contains("pid=102 pgid=200"));
        assert!(text.contains("state=unknown"));
        assert!(text.contains("WARNING: 1 running artesian-mcp process"));
    }

    #[test]
    fn restart_selection_targets_only_stale_mcp_processes() {
        let processes = vec![
            McpProcessStatus {
                process: RunningMcpProcess {
                    pid: 10,
                    pgid: Some(10),
                    exe: Some(PathBuf::from("/old/artesian")),
                    command: "artesian-mcp".to_string(),
                },
                binary_state: McpBinaryState::Stale,
            },
            McpProcessStatus {
                process: RunningMcpProcess {
                    pid: 11,
                    pgid: Some(10),
                    exe: Some(PathBuf::from("/old/artesian")),
                    command: "artesian-mcp".to_string(),
                },
                binary_state: McpBinaryState::Stale,
            },
            McpProcessStatus {
                process: RunningMcpProcess {
                    pid: 12,
                    pgid: Some(12),
                    exe: Some(PathBuf::from("/new/artesian")),
                    command: "artesian-mcp".to_string(),
                },
                binary_state: McpBinaryState::Current,
            },
            McpProcessStatus {
                process: RunningMcpProcess {
                    pid: 13,
                    pgid: None,
                    exe: None,
                    command: "artesian-mcp".to_string(),
                },
                binary_state: McpBinaryState::Unknown,
            },
            McpProcessStatus {
                process: RunningMcpProcess {
                    pid: 14,
                    pgid: Some(99),
                    exe: Some(PathBuf::from("/old/artesian")),
                    command: "artesian-mcp".to_string(),
                },
                binary_state: McpBinaryState::Stale,
            },
        ];

        let targets = stale_mcp_restart_targets(&processes);
        let pids = targets.iter().map(|target| target.pid).collect::<Vec<_>>();

        assert_eq!(pids, vec![10, 14]);
        assert_eq!(targets[0].mode, RestartMode::ProcessGroup(10));
        assert_eq!(targets[1].mode, RestartMode::ProcessOnly);
    }

    #[test]
    fn brew_absence_skips_upgrade_but_has_reportable_guidance() {
        assert_eq!(
            brew_upgrade_plan(false, false),
            BrewUpgradePlan::SkipUnavailable
        );

        let text = render_brew_update_outcome(&BrewUpdateOutcome::Unavailable);

        assert!(text.contains("Homebrew: not found"));
        assert!(text.contains("skipped the brew upgrade"));
        assert!(text.contains("inspect installed surfaces"));
    }
}
