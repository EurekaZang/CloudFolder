use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MOUNTS_DIR: &str = r"C:\ProgramData\CloudFolder\mounts";
const AGENT_BEGIN: &str = "<!-- CloudFolder agent instructions: begin -->";
const AGENT_END: &str = "<!-- CloudFolder agent instructions: end -->";
const AGENT_INSTRUCTIONS: &str = r#"## CloudFolder remote workspaces

When the current working directory is inside a CloudFolder mount:

- Use normal local filesystem tools to read, edit, search, create, rename, and delete workspace files.
- If unsure whether the current directory is a CloudFolder workspace, run `cf here`.
- Run Git, builds, tests, package managers, compilers, project interpreters, and other Linux/project commands on the remote host with `cf run -- <program> [args...]`.
- For repository-wide grep/find/search operations that touch many cold files, prefer remote tools such as `cf run -- rg ...` or `cf run -- find ...`; targeted file reads and edits can stay local.
- For pipelines, redirects, shell operators, or compound commands, use `cf sh -- "<shell command>"`.
- `cf run` waits for pending local writes, maps the local working directory to the matching remote Linux directory, executes there, and refreshes the mounted directory view afterward.
- Do not run a second coding agent on the remote host just to work on this workspace. The coding agent stays local; CloudFolder bridges files and remote execution.
- Keep commands intentionally targeting the local Windows machine local.

Direct local Git operations against a CloudFolder mount may be slow because Git performs many small random accesses inside `.git`; prefer `cf run -- git ...`.
"#;

#[derive(Debug, Clone, Deserialize)]
struct MountRecord {
    name: String,
    slug: String,
    service_name: String,
    host: String,
    port: u16,
    user: String,
    #[serde(default)]
    remote_path: String,
    #[serde(default)]
    remote_root: String,
    mount_point: String,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    rclone_config: String,
    #[serde(default)]
    key_file: String,
    #[serde(default)]
    known_hosts: String,
    rc_port: u16,
}

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => exit(code),
        Err(error) => {
            eprintln!("cf: {error:#}");
            exit(2);
        }
    }
}

fn dispatch(args: &[OsString]) -> Result<i32> {
    let command = args
        .first()
        .and_then(|arg| arg.to_str())
        .map(|value| value.to_ascii_lowercase());
    match command.as_deref() {
        None | Some("help" | "-h" | "--help") => {
            print_usage();
            Ok(0)
        }
        Some("list") => native_list(),
        Some("path") => native_path(&args[1..]),
        Some("here") => native_here(),
        Some("status") => native_status(&args[1..]),
        Some("flush") => native_flush(&args[1..]),
        Some("refresh") => native_refresh(&args[1..]),
        Some("run") => native_run(&args[1..]),
        Some("sh") => native_sh(&args[1..]),
        Some("shell") => native_shell(&args[1..]),
        Some("agent") => native_agent(&args[1..]),
        _ => launch_powershell(args),
    }
}

fn print_usage() {
    println!(
        "CloudFolder developer CLI\n\n\
  cf list\n\
  cf path <mount>\n\
  cf here\n\
  cf status [mount]\n\
  cf flush [mount]\n\
  cf refresh [mount]\n\
  cf run [mount] -- <program> [args...]\n\
  cf sh [mount] -- <shell command>\n\
  cf shell [mount]\n\
  cf agent setup|status|remove\n\n\
Examples:\n\
  cd (cf path lab)\n\
  cf run -- git status\n\
  cf run -- pytest -q\n\
  cf sh -- \"git status && pytest -q\"\n\
  cf shell\n\
  cf agent setup"
    );
}

fn native_list() -> Result<i32> {
    let records = load_mounts()?;
    if records.is_empty() {
        println!("No CloudFolder mounts are configured.");
        return Ok(0);
    }
    for record in records {
        let profile = if record.profile.trim().is_empty() {
            "Legacy"
        } else {
            record.profile.as_str()
        };
        println!(
            "{}\t{}\t{}\t{}@{}:{}",
            record.name, record.mount_point, profile, record.user, record.host, record.port
        );
    }
    Ok(0)
}

fn native_path(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    println!("{}", record.mount_point);
    Ok(0)
}

fn native_here() -> Result<i32> {
    let record = resolve_mount(None, true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote = remote_working_directory(&record, &cwd)?;
    let profile = if record.profile.trim().is_empty() {
        "Legacy"
    } else {
        record.profile.as_str()
    };
    println!("Mount:      {}", record.name);
    println!("Profile:    {profile}");
    println!("Local root: {}", record.mount_point);
    println!("Local cwd:  {}", cwd.display());
    println!("Remote cwd: {remote}");
    Ok(0)
}

fn native_status(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let service = service_state(&record.service_name);
    let mounted = Path::new(&record.mount_point).exists();
    let pending = match rc_json(&record, "vfs/stats") {
        Ok(stats) => {
            let disk = stats.get("diskCache");
            let queued = disk
                .and_then(|value| value.get("uploadsQueued"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let active = disk
                .and_then(|value| value.get("uploadsInProgress"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (queued + active).to_string()
        }
        Err(_) => "RC unavailable".to_string(),
    };
    let profile = if record.profile.trim().is_empty() {
        "Legacy"
    } else {
        record.profile.as_str()
    };
    println!("Name:           {}", record.name);
    println!("Profile:        {profile}");
    println!("Service:        {service}");
    println!("Mounted:        {mounted}");
    println!("Pending writes: {pending}");
    println!("Local root:     {}", record.mount_point);
    println!("Remote root:    {}", resolve_remote_root(&record)?);
    Ok(if service == "Running" && mounted {
        0
    } else {
        1
    })
}

fn native_agent(args: &[OsString]) -> Result<i32> {
    let action = args
        .first()
        .and_then(|arg| arg.to_str())
        .unwrap_or("status")
        .to_ascii_lowercase();
    if args.len() > 1 {
        bail!("usage: cf agent setup|status|remove");
    }
    let files = agent_instruction_files()?;
    match action.as_str() {
        "setup" => {
            for (label, path) in &files {
                install_agent_block(path)?;
                println!("Configured {label}: {}", path.display());
            }
            println!(
                "CloudFolder agent guidance is enabled. Start a new Claude Code or Codex session inside the mounted workspace."
            );
            Ok(0)
        }
        "remove" => {
            for (label, path) in &files {
                remove_agent_block(path)?;
                println!(
                    "Removed CloudFolder guidance from {label}: {}",
                    path.display()
                );
            }
            Ok(0)
        }
        "status" => {
            for (label, path) in &files {
                let enabled = read_text_utf8(path)
                    .map(|text| text.contains(AGENT_BEGIN) && text.contains(AGENT_END))
                    .unwrap_or(false);
                println!(
                    "{label}: {} ({})",
                    path.display(),
                    if enabled { "enabled" } else { "not configured" }
                );
            }
            Ok(0)
        }
        _ => bail!("usage: cf agent setup|status|remove"),
    }
}

fn native_flush(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    wait_for_flush(&record, Duration::from_secs(60))?;
    println!("Flushed: {}", record.name);
    Ok(0)
}

fn native_refresh(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    refresh_vfs(&record)?;
    println!("Refreshed: {}", record.name);
    Ok(0)
}

fn native_run(args: &[OsString]) -> Result<i32> {
    let (requested, command) = split_remote_command(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote_cwd = remote_working_directory(&record, &cwd)?;
    wait_for_flush(&record, Duration::from_secs(60))?;

    let mut remote_command = format!("cd -- {} && exec", quote_posix(&remote_cwd));
    for arg in command {
        remote_command.push(' ');
        remote_command.push_str(&quote_posix(&arg.to_string_lossy()));
    }
    let code = run_ssh(&record, false, &remote_command)?;
    let _ = refresh_vfs(&record);
    Ok(code)
}

fn native_sh(args: &[OsString]) -> Result<i32> {
    let (requested, command) = split_remote_command(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote_cwd = remote_working_directory(&record, &cwd)?;
    wait_for_flush(&record, Duration::from_secs(60))?;
    let shell_text = command
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let remote_command = format!("cd -- {} && {shell_text}", quote_posix(&remote_cwd));
    let code = run_ssh(&record, false, &remote_command)?;
    let _ = refresh_vfs(&record);
    Ok(code)
}

fn native_shell(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote_cwd = remote_working_directory(&record, &cwd)?;
    wait_for_flush(&record, Duration::from_secs(60))?;
    let remote_command = format!(
        "cd -- {} && exec ${{SHELL:-/bin/sh}} -l",
        quote_posix(&remote_cwd)
    );
    let code = run_ssh(&record, true, &remote_command)?;
    let _ = refresh_vfs(&record);
    Ok(code)
}

fn launch_powershell(args: &[OsString]) -> Result<i32> {
    let script = launcher_script()?;
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .args(args)
        .status()
        .context("failed to start PowerShell")?;
    Ok(status.code().unwrap_or(1))
}

fn launcher_script() -> Result<PathBuf> {
    let exe = env::current_exe().context("cannot locate cf.exe")?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow!("cannot locate the CloudFolder install directory"))?;
    let script = parent.join("cf.ps1");
    if !script.is_file() {
        bail!("missing {}", script.display());
    }
    Ok(script)
}

fn service_state(service_name: &str) -> String {
    let output = Command::new("sc.exe")
        .args(["query", service_name])
        .output();
    let Ok(output) = output else {
        return "Unavailable".to_string();
    };
    if !output.status.success() {
        return "Missing".to_string();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.contains("RUNNING") {
        "Running".to_string()
    } else if text.contains("STOPPED") {
        "Stopped".to_string()
    } else if text.contains("START_PENDING") {
        "Starting".to_string()
    } else if text.contains("STOP_PENDING") {
        "Stopping".to_string()
    } else {
        "Unknown".to_string()
    }
}

fn agent_instruction_files() -> Result<Vec<(&'static str, PathBuf)>> {
    let home = env::var_os("CLOUDFOLDER_AGENT_HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| anyhow!("cannot locate the Windows user profile"))?;
    let home = PathBuf::from(home);
    Ok(vec![
        ("Claude Code", home.join(".claude").join("CLAUDE.md")),
        ("Codex", home.join(".codex").join("AGENTS.md")),
    ])
}

fn read_text_utf8(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec()).with_context(|| {
        format!(
            "{} is not UTF-8; CloudFolder will not rewrite an instruction file with an unknown encoding",
            path.display()
        )
    })
}

fn managed_agent_block(newline: &str) -> String {
    let instructions = AGENT_INSTRUCTIONS.trim_end().replace('\n', newline);
    format!("{AGENT_BEGIN}{newline}{instructions}{newline}{AGENT_END}")
}

fn upsert_managed_block(text: &str) -> Result<String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block = managed_agent_block(newline);
    let start = text.find(AGENT_BEGIN);
    let end = text.find(AGENT_END);
    match (start, end) {
        (Some(start), Some(end)) if end >= start => {
            let end = end + AGENT_END.len();
            Ok(format!("{}{}{}", &text[..start], block, &text[end..]))
        }
        (None, None) => {
            if text.is_empty() {
                Ok(format!("{block}{newline}"))
            } else {
                let prefix = text.trim_end_matches(['\r', '\n']);
                Ok(format!("{prefix}{newline}{newline}{block}{newline}"))
            }
        }
        _ => bail!("found an incomplete CloudFolder managed instruction block; repair the markers manually before retrying"),
    }
}

fn remove_managed_block_text(text: &str) -> Result<String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let start = text.find(AGENT_BEGIN);
    let end = text.find(AGENT_END);
    match (start, end) {
        (None, None) => Ok(text.to_string()),
        (Some(start), Some(end)) if end >= start => {
            let end = end + AGENT_END.len();
            let before = text[..start].trim_end_matches(['\r', '\n']);
            let after = text[end..].trim_start_matches(['\r', '\n']);
            match (before.is_empty(), after.is_empty()) {
                (true, true) => Ok(String::new()),
                (false, true) => Ok(format!("{before}{newline}")),
                (true, false) => Ok(after.to_string()),
                (false, false) => Ok(format!("{before}{newline}{newline}{after}")),
            }
        }
        _ => bail!("found an incomplete CloudFolder managed instruction block; repair the markers manually before retrying"),
    }
}

fn install_agent_block(path: &Path) -> Result<()> {
    let existing = read_text_utf8(path)?;
    let updated = upsert_managed_block(&existing)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(path, updated.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

fn remove_agent_block(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let existing = read_text_utf8(path)?;
    let updated = remove_managed_block_text(&existing)?;
    fs::write(path, updated.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

fn load_mounts() -> Result<Vec<MountRecord>> {
    let root = Path::new(MOUNTS_DIR);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("cannot read {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let metadata = entry.path().join("mount.json");
        if !metadata.is_file() {
            continue;
        }
        let bytes =
            fs::read(&metadata).with_context(|| format!("cannot read {}", metadata.display()))?;
        let record = parse_mount_record(&bytes)
            .with_context(|| format!("invalid {}", metadata.display()))?;
        records.push(record);
    }
    records.sort_by_key(|record| record.name.to_lowercase());
    Ok(records)
}

fn parse_mount_record(bytes: &[u8]) -> Result<MountRecord> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return serde_json::from_slice(rest).context("invalid UTF-8 JSON");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        if rest.len() % 2 != 0 {
            bail!("invalid UTF-16LE JSON length");
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16(&units).context("invalid UTF-16LE JSON")?;
        return serde_json::from_str(&text).context("invalid UTF-16LE JSON");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        if rest.len() % 2 != 0 {
            bail!("invalid UTF-16BE JSON length");
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16(&units).context("invalid UTF-16BE JSON")?;
        return serde_json::from_str(&text).context("invalid UTF-16BE JSON");
    }
    serde_json::from_slice(bytes).context("invalid UTF-8 JSON")
}

fn resolve_mount(requested: Option<&str>, use_cwd: bool) -> Result<MountRecord> {
    let records = load_mounts()?;
    if records.is_empty() {
        bail!("no CloudFolder mounts are configured");
    }
    if let Some(name) = requested.filter(|value| !value.trim().is_empty()) {
        let matches: Vec<_> = records
            .iter()
            .filter(|record| {
                record.name.eq_ignore_ascii_case(name)
                    || record.slug.eq_ignore_ascii_case(name)
                    || record.service_name.eq_ignore_ascii_case(name)
            })
            .cloned()
            .collect();
        return match matches.as_slice() {
            [record] => Ok(record.clone()),
            [] => bail!("could not find CloudFolder mount '{name}'"),
            _ => bail!("mount name '{name}' is ambiguous"),
        };
    }
    if use_cwd {
        let cwd = env::current_dir().context("cannot read the current directory")?;
        let mut candidates: Vec<_> = records
            .iter()
            .filter_map(|record| {
                relative_components(&cwd, Path::new(&record.mount_point))
                    .map(|relative| (record.clone(), relative.len()))
            })
            .collect();
        candidates.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));
        if let Some((record, _)) = candidates.into_iter().next() {
            return Ok(record);
        }
    }
    if records.len() == 1 {
        return Ok(records[0].clone());
    }
    bail!("the current directory is not inside a CloudFolder mount; pass the mount name explicitly")
}

fn relative_components(candidate: &Path, root: &Path) -> Option<Vec<OsString>> {
    let candidate_parts = comparable_components(candidate);
    let root_parts = comparable_components(root);
    if candidate_parts.len() < root_parts.len() {
        return None;
    }
    for (candidate_part, root_part) in candidate_parts.iter().zip(root_parts.iter()) {
        if !candidate_part.eq_ignore_ascii_case(root_part) {
            return None;
        }
    }
    Some(
        candidate
            .components()
            .skip(root.components().count())
            .filter_map(component_to_os)
            .collect(),
    )
}

fn comparable_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(value) => Some(value.as_os_str().to_string_lossy().to_string()),
            Component::RootDir => Some("\\".to_string()),
            Component::CurDir => None,
            Component::ParentDir => Some("..".to_string()),
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
        })
        .collect()
}

fn component_to_os(component: Component<'_>) -> Option<OsString> {
    match component {
        Component::Normal(value) => Some(value.to_os_string()),
        Component::ParentDir => Some(OsString::from("..")),
        _ => None,
    }
}

fn remote_working_directory(record: &MountRecord, cwd: &Path) -> Result<String> {
    let root = resolve_remote_root(record)?;
    let Some(relative) = relative_components(cwd, Path::new(&record.mount_point)) else {
        return Ok(root);
    };
    if relative.is_empty() {
        return Ok(root);
    }
    let suffix = relative
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if root == "/" {
        Ok(format!("/{suffix}"))
    } else {
        Ok(format!("{}/{suffix}", root.trim_end_matches('/')))
    }
}

fn resolve_remote_root(record: &MountRecord) -> Result<String> {
    if !record.remote_root.trim().is_empty() {
        return Ok(normalize_remote_root(&record.remote_root));
    }
    if record.remote_path.starts_with('/') {
        return Ok(normalize_remote_root(&record.remote_path));
    }
    let command = if record.remote_path.trim().is_empty() || record.remote_path == "~" {
        "pwd -P".to_string()
    } else {
        let relative = record
            .remote_path
            .strip_prefix("~/")
            .unwrap_or(&record.remote_path);
        format!("cd -- {} && pwd -P", quote_posix(relative))
    };
    let output = ssh_command(record)?
        .arg(format!("{}@{}", record.user, record.host))
        .arg(command)
        .output()
        .context("failed to resolve the remote root through SSH")?;
    if !output.status.success() {
        bail!("could not resolve the remote root for '{}'", record.name);
    }
    let root = String::from_utf8(output.stdout)
        .context("remote root was not valid UTF-8")?
        .trim()
        .to_string();
    if !root.starts_with('/') {
        bail!("remote root is not an absolute Linux path: {root}");
    }
    Ok(normalize_remote_root(&root))
}

fn normalize_remote_root(value: &str) -> String {
    if value == "/" {
        "/".to_string()
    } else {
        value.trim_end_matches('/').to_string()
    }
}

fn run_ssh(record: &MountRecord, tty: bool, remote_command: &str) -> Result<i32> {
    let mut command = ssh_command(record)?;
    if tty {
        command.arg("-t");
    }
    command
        .arg(format!("{}@{}", record.user, record.host))
        .arg(remote_command);
    let status = command
        .status()
        .context("failed to start Windows OpenSSH")?;
    Ok(status.code().unwrap_or(1))
}

fn ssh_command(record: &MountRecord) -> Result<Command> {
    let (key_file, known_hosts) = ssh_files(record)?;
    let mut command = Command::new("ssh.exe");
    command
        .arg("-p")
        .arg(record.port.to_string())
        .arg("-i")
        .arg(key_file)
        .args(["-o", "BatchMode=yes"])
        .args(["-o", "IdentitiesOnly=yes"])
        .args(["-o", "StrictHostKeyChecking=yes"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .args(["-o", "ServerAliveInterval=15"])
        .args(["-o", "ServerAliveCountMax=3"]);
    Ok(command)
}

fn ssh_files(record: &MountRecord) -> Result<(PathBuf, PathBuf)> {
    let mut key = record.key_file.trim().to_string();
    let mut known_hosts = record.known_hosts.trim().to_string();
    if (key.is_empty() || known_hosts.is_empty()) && !record.rclone_config.trim().is_empty() {
        let text = fs::read_to_string(&record.rclone_config)
            .with_context(|| format!("cannot read {}", record.rclone_config))?;
        if key.is_empty() {
            key = ini_value(&text, "key_file").unwrap_or_default();
        }
        if known_hosts.is_empty() {
            known_hosts = ini_value(&text, "known_hosts_file").unwrap_or_default();
        }
    }
    if key.is_empty() || known_hosts.is_empty() {
        bail!(
            "mount '{}' is missing SSH key metadata; re-add the mount",
            record.name
        );
    }
    let key = PathBuf::from(key);
    let known_hosts = PathBuf::from(known_hosts);
    if !key.is_file() {
        bail!("SSH key does not exist: {}", key.display());
    }
    if !known_hosts.is_file() {
        bail!("known_hosts does not exist: {}", known_hosts.display());
    }
    Ok((key, known_hosts))
}

fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (left, right) = line.split_once('=')?;
        if left.trim().eq_ignore_ascii_case(key) {
            Some(right.trim().to_string())
        } else {
            None
        }
    })
}

fn wait_for_flush(record: &MountRecord, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let stats = rc_json(record, "vfs/stats")?;
        let disk = stats.get("diskCache");
        let queued = disk
            .and_then(|value| value.get("uploadsQueued"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let active = disk
            .and_then(|value| value.get("uploadsInProgress"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if queued == 0 && active == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for pending writes on '{}' (queued={queued}, in_progress={active})",
                record.name
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn refresh_vfs(record: &MountRecord) -> Result<()> {
    let _ = rc_json(record, "vfs/forget")?;
    Ok(())
}

fn rc_json(record: &MountRecord, method: &str) -> Result<Value> {
    let rclone = installed_sibling("rclone.exe")?;
    let url = format!("http://127.0.0.1:{}/", record.rc_port);
    let output = Command::new(rclone)
        .args(["rc", "--url"])
        .arg(url)
        .arg(method)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to query CloudFolder RC method {method}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("RC method {method} failed: {}", stderr.trim());
    }
    if output.stdout.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("invalid RC response for {method}"))
}

fn installed_sibling(name: &str) -> Result<PathBuf> {
    let exe = env::current_exe().context("cannot locate cf.exe")?;
    let path = exe
        .parent()
        .ok_or_else(|| anyhow!("cannot locate the CloudFolder install directory"))?
        .join(name);
    if !path.is_file() {
        bail!("missing {}", path.display());
    }
    Ok(path)
}

fn split_remote_command(args: &[OsString]) -> Result<(Option<String>, Vec<OsString>)> {
    let delimiter = args
        .iter()
        .position(|value| value == OsStr::new("--"))
        .ok_or_else(|| anyhow!("expected -- before the remote command"))?;
    let before = &args[..delimiter];
    let command = args[delimiter + 1..].to_vec();
    if before.len() > 1 {
        bail!("pass at most one mount name before --");
    }
    if command.is_empty() {
        bail!("a remote command is required after --");
    }
    let requested = before
        .first()
        .map(|value| value.to_string_lossy().to_string());
    Ok((requested, command))
}

fn single_optional_name(args: &[OsString]) -> Result<Option<String>> {
    if args.len() > 1 {
        bail!("pass at most one mount name");
    }
    Ok(args
        .first()
        .map(|value| value.to_string_lossy().to_string()))
}

fn quote_posix(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_path_is_relative_to_executable() {
        if let Ok(path) = launcher_script() {
            assert_eq!(path.file_name().unwrap(), "cf.ps1");
        }
    }

    #[test]
    fn shell_quoting_handles_spaces_quotes_and_unicode() {
        assert_eq!(quote_posix("hello world"), "'hello world'");
        assert_eq!(quote_posix("apostrophe's"), "'apostrophe'\\''s'");
        assert_eq!(quote_posix("中文\"x"), "'中文\"x'");
    }

    #[test]
    fn command_split_supports_optional_mount() {
        let args = vec![
            OsString::from("lab"),
            OsString::from("--"),
            OsString::from("pytest"),
            OsString::from("-q"),
        ];
        let (mount, command) = split_remote_command(&args).unwrap();
        assert_eq!(mount.as_deref(), Some("lab"));
        assert_eq!(
            command,
            vec![OsString::from("pytest"), OsString::from("-q")]
        );
    }

    #[test]
    fn mount_json_accepts_utf8_bom() {
        let json = br#"{"name":"x","slug":"x","service_name":"CloudFolder.x","host":"example.com","port":22,"user":"alice","mount_point":"C:\\CloudFolder\\x","rc_port":55770}"#;
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(json);
        let record = parse_mount_record(&bytes).unwrap();
        assert_eq!(record.name, "x");
    }

    #[test]
    fn managed_agent_block_preserves_existing_content() {
        let original = "# My instructions\r\n\r\nKeep this.\r\n";
        let installed = upsert_managed_block(original).unwrap();
        assert!(installed.starts_with("# My instructions\r\n\r\nKeep this."));
        assert!(installed.contains(AGENT_BEGIN));
        assert!(installed.contains("cf run --"));
        let removed = remove_managed_block_text(&installed).unwrap();
        assert_eq!(removed, original);
    }

    #[test]
    fn managed_agent_block_updates_without_duplication() {
        let first = upsert_managed_block("").unwrap();
        let second = upsert_managed_block(&first).unwrap();
        assert_eq!(second.matches(AGENT_BEGIN).count(), 1);
        assert_eq!(second.matches(AGENT_END).count(), 1);
    }
}
