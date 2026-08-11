use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const MOUNTS_DIR: &str = r"C:\ProgramData\CloudFolder\mounts";
const DATA_DIR: &str = r"C:\ProgramData\CloudFolder";
const AGENT_BEGIN: &str = "<!-- CloudFolder agent instructions: begin -->";
const AGENT_END: &str = "<!-- CloudFolder agent instructions: end -->";
const ROUTED_TOOLS: &[&str] = &[
    "git",
    "python",
    "python3",
    "pytest",
    "uv",
    "pip",
    "pip3",
    "conda",
    "cargo",
    "rustc",
    "gcc",
    "g++",
    "clang",
    "clang++",
    "cmake",
    "make",
    "ninja",
    "node",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "go",
    "java",
    "javac",
    "mvn",
    "gradle",
    "rg",
    "find",
    "bash",
    "sh",
    "nvidia-smi",
];
const AGENT_INSTRUCTIONS: &str = r#"## CloudFolder remote workspaces

When the current working directory is inside a CloudFolder mount:

- Use normal local filesystem tools to read, edit, search, create, rename, and delete workspace files.
- If unsure whether the current directory is a CloudFolder workspace, run `cf here`.
- Prefer starting the agent from `cf enter <mount>` (or run `cf enter` while already inside the mount). In that session Git, Python, tests, package managers, compilers, and common Linux tooling are transparently routed to the matching remote runtime, so plain commands such as `git status`, `pytest`, and `python train.py` are correct.
- For repository-wide grep/find/search operations that touch many cold files, use the routed remote tools; targeted file reads and edits stay local through the mounted Windows path.
- If a command must explicitly bypass routing and target the Windows machine, invoke the Windows executable by absolute path or start it outside `cf enter`.
- Legacy `cf run` / `cf sh` remain available for scripts and explicit automation, but normal interactive and agent workflows should not need them after `cf enter`.
- Workspace runtime state from `.cloudfolder.toml` is applied automatically to routed commands, explicit remote commands, and persistent jobs.
- Do not run a second coding agent on the remote host just to work on this workspace. The coding agent stays local; CloudFolder bridges files and remote execution.
- Keep commands intentionally targeting the local Windows machine local.

Outside a `cf enter` session, direct local Git operations against a CloudFolder mount may still be slow because Git performs many small random accesses inside `.git`; enter the routed runtime before repository-wide CLI work.
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
    #[serde(default)]
    ssh_alias: String,
    #[serde(default)]
    ssh_config: String,
    rc_port: u16,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct WorkspaceConfig {
    #[serde(default)]
    environment: EnvironmentConfig,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct EnvironmentConfig {
    #[serde(default)]
    shell: String,
    #[serde(default)]
    init: String,
    #[serde(default)]
    active: String,
    #[serde(default)]
    profiles: BTreeMap<String, EnvironmentProfile>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct EnvironmentProfile {
    #[serde(default)]
    shell: String,
    #[serde(default)]
    init: String,
}

#[derive(Debug, Clone)]
struct EffectiveEnvironment {
    config_path: Option<PathBuf>,
    shell: String,
    init: String,
    active: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ForwardState {
    mount_slug: String,
    remote_port: u16,
    local_port: u16,
    pid: u32,
    started_epoch: u64,
}

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let invoked_tool = env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|value| value.to_string_lossy().to_string())
        })
        .and_then(|name| routed_tool_from_exe_name(&name));
    let result = match invoked_tool {
        Some(tool) => native_routed_tool(&tool, &args),
        None => dispatch(&args),
    };
    match result {
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
        Some("version" | "-v" | "--version") => {
            println!("CloudFolder {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("list") => native_list(),
        Some("path") => native_path(&args[1..]),
        Some("here") => native_here(),
        Some("status") => native_status(&args[1..]),
        Some("enter") => native_enter(&args[1..]),
        Some("env") => native_env(&args[1..]),
        Some("job") => native_job(&args[1..]),
        Some("forward") => native_forward(&args[1..]),
        Some("add") => native_add(&args[1..]),
        Some("ssh-proxy") => native_ssh_proxy(&args[1..]),
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
  cf enter [mount]\n\
  cf env [use <profile>|reload]\n\
  cf job run [mount] -- <program> [args...]\n\
  cf job list [mount]\n\
  cf job logs [-f] <job> [--mount <mount>]\n\
  cf job stop <job> [--mount <mount>]\n\
  cf job attach <job> [--mount <mount>]\n\
  cf forward <remote-port> [local-port] [--mount <mount>]\n\
  cf forward list [mount]\n\
  cf forward stop <local-port|all> [--mount <mount>]\n\
  cf add <ssh-config-host>\n\
  cf flush [mount]\n\
  cf refresh [mount]\n\
  cf run [mount] -- <program> [args...]\n\
  cf sh [mount] -- <shell command>\n\
  cf shell [mount]\n\
  cf agent setup|status|remove\n\n\
Examples:\n\
  cd (cf path lab)\n\
  cf enter\n\
  git status\n\
  pytest -q\n\
  cf job run -- python train.py\n\
  cf forward 8888\n\
  cf add h100\n\
  cf run -- git status\n\
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

fn native_enter(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let start_dir = if relative_components(&cwd, Path::new(&record.mount_point)).is_some() {
        cwd
    } else {
        PathBuf::from(&record.mount_point)
    };
    let router = ensure_router_shims()?;
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![router.clone()];
    paths.extend(env::split_paths(&current_path));
    let routed_path = env::join_paths(paths).context("cannot construct routed PATH")?;

    println!("Entering CloudFolder runtime: {}", record.name);
    println!("Local workspace: {}", start_dir.display());
    println!("Remote root: {}", resolve_remote_root(&record)?);
    println!("Transparent remote tools: {}", ROUTED_TOOLS.join(", "));
    println!("Local tools such as cd/dir/explorer/code remain local.");

    let status = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoExit"])
        .env("CLOUDFOLDER_ENTER_MOUNT", &record.slug)
        .env("CLOUDFOLDER_ROUTER_ACTIVE", "1")
        .env("CLOUDFOLDER_RUNTIME_DIR", runtime_dir()?)
        .env("PATH", routed_path)
        .current_dir(start_dir)
        .status()
        .context("failed to start the routed PowerShell session")?;
    Ok(status.code().unwrap_or(1))
}

fn native_routed_tool(tool: &str, args: &[OsString]) -> Result<i32> {
    let requested = env::var("CLOUDFOLDER_ENTER_MOUNT").ok();
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    if matches!(tool, "bash" | "sh") && args.is_empty() {
        let remote_cwd = remote_working_directory(&record, &cwd)?;
        let environment = effective_environment(&record, &cwd)?;
        wait_for_flush(&record, Duration::from_secs(60))?;
        let mut body = format!("set -e\ncd -- {}\n", quote_posix(&remote_cwd));
        if !environment.init.trim().is_empty() {
            body.push_str(environment.init.trim_end());
            body.push('\n');
        }
        body.push_str("set +e\n");
        body.push_str(&format!("exec {tool} -l"));
        let remote_command = wrap_environment_shell(&environment, &body);
        let code = run_ssh(&record, true, &remote_command)?;
        let _ = refresh_vfs(&record);
        return Ok(code);
    }
    let mut command = Vec::with_capacity(args.len() + 1);
    command.push(OsString::from(tool));
    command.extend_from_slice(args);
    execute_remote_argv(&record, &cwd, &command)
}

fn routed_tool_from_exe_name(name: &str) -> Option<String> {
    let normalized = name.to_ascii_lowercase();
    ROUTED_TOOLS
        .iter()
        .find(|tool| tool.eq_ignore_ascii_case(&normalized))
        .map(|value| (*value).to_string())
}

fn router_bin_dir() -> PathBuf {
    env::var_os("CLOUDFOLDER_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DATA_DIR))
        .join("router")
        .join(env!("CARGO_PKG_VERSION"))
        .join("bin")
}

fn ensure_router_shims() -> Result<PathBuf> {
    let source = env::current_exe().context("cannot locate cf.exe")?;
    let dir = router_bin_dir();
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for tool in ROUTED_TOOLS {
        let path = dir.join(format!("{tool}.exe"));
        if path.exists() {
            continue;
        }
        if fs::hard_link(&source, &path).is_err() {
            fs::copy(&source, &path)
                .with_context(|| format!("cannot create router shim {}", path.display()))?;
        }
    }
    Ok(dir)
}

fn native_env(args: &[OsString]) -> Result<i32> {
    let entered = env::var("CLOUDFOLDER_ENTER_MOUNT").ok();
    let record = resolve_mount(entered.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    match args.first().and_then(|arg| arg.to_str()) {
        None => {
            print_effective_environment(&record, &cwd)?;
            Ok(0)
        }
        Some("reload") if args.len() == 1 => {
            let environment = effective_environment(&record, &cwd)?;
            println!(
                "Environment config reloaded: {}",
                environment
                    .config_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "defaults (no .cloudfolder.toml)".to_string())
            );
            println!("Commands read this environment on every invocation; no daemon restart is required.");
            Ok(0)
        }
        Some("use") if args.len() == 2 => {
            let profile = args[1].to_string_lossy().to_string();
            set_environment_profile(&record, &cwd, &profile)?;
            println!("Active CloudFolder environment profile: {profile}");
            Ok(0)
        }
        _ => bail!("usage: cf env [use <profile>|reload]"),
    }
}

fn print_effective_environment(record: &MountRecord, cwd: &Path) -> Result<()> {
    let environment = effective_environment(record, cwd)?;
    println!(
        "Config:  {}",
        environment
            .config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(none)".to_string())
    );
    println!(
        "Profile: {}",
        if environment.active.is_empty() {
            "(base)"
        } else {
            environment.active.as_str()
        }
    );
    println!(
        "Shell:   {}",
        if environment.shell.is_empty() {
            "(remote default shell)"
        } else {
            environment.shell.as_str()
        }
    );
    if environment.init.trim().is_empty() {
        println!("Init:    (none)");
    } else {
        println!("Init:\n{}", environment.init.trim_end());
    }
    Ok(())
}

fn native_add(args: &[OsString]) -> Result<i32> {
    if args.len() != 1 {
        bail!("usage: cf add <ssh-config-host>");
    }
    let host = args[0].to_string_lossy().to_string();
    if host.trim().is_empty() || host.starts_with('-') {
        bail!("usage: cf add <ssh-config-host>");
    }
    launch_manager_powershell(&[
        OsString::from("-Action"),
        OsString::from("Add"),
        OsString::from("-SshHost"),
        OsString::from(host),
        OsString::from("-NonInteractive"),
        OsString::from("-NoOpen"),
    ])
}

fn native_ssh_proxy(args: &[OsString]) -> Result<i32> {
    let delimiter = args
        .iter()
        .position(|arg| arg == OsStr::new("--"))
        .ok_or_else(|| anyhow!("internal ssh-proxy invocation is missing --"))?;
    let control = &args[..delimiter];
    let passthrough = &args[delimiter + 1..];
    let mut home = None;
    let mut config = None;
    let mut target = None;
    let mut index = 0;
    while index < control.len() {
        let flag = control[index].to_string_lossy();
        let value = control
            .get(index + 1)
            .ok_or_else(|| anyhow!("internal ssh-proxy option {flag} is missing a value"))?;
        match flag.as_ref() {
            "--home" => home = Some(PathBuf::from(value)),
            "--config" => config = Some(PathBuf::from(value)),
            "--target" => target = Some(value.to_string_lossy().to_string()),
            _ => bail!("unknown internal ssh-proxy option {flag}"),
        }
        index += 2;
    }
    let home = home.ok_or_else(|| anyhow!("internal ssh-proxy home is missing"))?;
    let target = target.ok_or_else(|| anyhow!("internal ssh-proxy target is missing"))?;
    let mut command = Command::new("ssh.exe");
    if let Some(config) = config.filter(|path| !path.as_os_str().is_empty()) {
        command.arg("-F").arg(config);
    }
    command
        .args(["-o", "BatchMode=yes"])
        .env("USERPROFILE", &home)
        .env("HOME", &home);
    let subsystem = passthrough
        .first()
        .is_some_and(|arg| arg == OsStr::new("-s"));
    if !subsystem {
        command.arg("-n");
    }
    command.arg(target).args(passthrough);
    let status = command
        .status()
        .context("CloudFolder ssh-proxy could not start Windows OpenSSH")?;
    Ok(status.code().unwrap_or(1))
}

fn native_job(args: &[OsString]) -> Result<i32> {
    let action = args
        .first()
        .and_then(|arg| arg.to_str())
        .unwrap_or("list")
        .to_ascii_lowercase();
    match action.as_str() {
        "run" => job_run(&args[1..]),
        "list" => job_list(&args[1..]),
        "logs" => job_logs(&args[1..], false),
        "attach" => job_logs(&args[1..], true),
        "stop" => job_stop(&args[1..]),
        _ => bail!("usage: cf job run|list|logs|attach|stop"),
    }
}

fn job_run(args: &[OsString]) -> Result<i32> {
    let (requested, command) = split_remote_command(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote_cwd = remote_working_directory(&record, &cwd)?;
    let environment = effective_environment(&record, &cwd)?;
    wait_for_flush(&record, Duration::from_secs(60))?;
    let id = new_job_id();
    let job_dir = format!("$HOME/.cloudfolder/jobs/{id}");
    let command_text = command
        .iter()
        .map(|arg| quote_posix(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let mut body = format!("set -e\ncd -- {}\n", quote_posix(&remote_cwd));
    if !environment.init.trim().is_empty() {
        body.push_str(environment.init.trim_end());
        body.push('\n');
    }
    body.push_str("set +e\n");
    body.push_str(&format!(
        "{command_text}\nrc=$?\nprintf '%s\\n' \"$rc\" > {job_dir}/exit_code\nprintf 'exited\\n' > {job_dir}/state\nexit \"$rc\""
    ));
    let body = wrap_environment_shell(&environment, &body);
    let display_command = command
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let remote = format!(
        "set -eu; d={job_dir}; mkdir -p \"$d\"; printf '%s\\n' {} > \"$d/cwd\"; printf '%s\\n' {} > \"$d/command\"; date +%s > \"$d/started\"; : > \"$d/stdout.log\"; if command -v setsid >/dev/null 2>&1; then nohup setsid sh -lc {} > \"$d/stdout.log\" 2>&1 < /dev/null & printf 'setsid\\n' > \"$d/mode\"; else nohup sh -lc {} > \"$d/stdout.log\" 2>&1 < /dev/null & printf 'plain\\n' > \"$d/mode\"; fi; p=$!; printf '%s\\n' \"$p\" > \"$d/pid\"; printf 'running\\n' > \"$d/state\"; printf '%s\\t%s\\n' {} \"$p\"",
        quote_posix(&remote_cwd),
        quote_posix(&display_command),
        quote_posix(&body),
        quote_posix(&body),
        quote_posix(&id),
    );
    let (code, stdout, stderr) = run_ssh_capture(&record, &remote)?;
    if code != 0 {
        bail!("could not start persistent job: {}", stderr.trim());
    }
    print!("{stdout}");
    println!(
        "Job {id} is detached; it continues if this SSH session or local computer disconnects."
    );
    Ok(0)
}

fn job_list(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let remote = r#"root="$HOME/.cloudfolder/jobs"; [ -d "$root" ] || exit 0; find "$root" -mindepth 1 -maxdepth 1 -type d -name 'cf-*' -print 2>/dev/null | while IFS= read -r d; do id=$(basename "$d"); pid=$(cat "$d/pid" 2>/dev/null || true); state=$(cat "$d/state" 2>/dev/null || echo unknown); if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then state=running; elif [ -f "$d/exit_code" ]; then state="exited($(cat "$d/exit_code"))"; elif [ "$state" = running ]; then state=unknown; fi; started=$(cat "$d/started" 2>/dev/null || echo '?'); cmd=$(cat "$d/command" 2>/dev/null || echo '?'); printf '%s\t%s\t%s\t%s\n' "$id" "$state" "$started" "$cmd"; done"#;
    let (code, stdout, stderr) = run_ssh_capture(&record, remote)?;
    if code != 0 {
        bail!("could not list jobs: {}", stderr.trim());
    }
    if stdout.trim().is_empty() {
        println!("No CloudFolder jobs on {}.", record.name);
    } else {
        println!("Job\tState\tStarted(epoch)\tCommand");
        print!("{stdout}");
    }
    Ok(0)
}

fn job_logs(args: &[OsString], force_follow: bool) -> Result<i32> {
    let (requested, rest) = extract_mount_flag(args)?;
    let mut follow = force_follow;
    let mut id = None;
    for arg in rest {
        if arg == OsStr::new("-f") || arg == OsStr::new("--follow") {
            follow = true;
        } else if id.is_none() {
            id = Some(arg.to_string_lossy().to_string());
        } else {
            bail!("usage: cf job logs [-f] <job> [--mount <mount>]");
        }
    }
    let id = id.ok_or_else(|| anyhow!("a job id is required"))?;
    validate_job_id(&id)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let path = format!("$HOME/.cloudfolder/jobs/{id}/stdout.log");
    let remote = if follow {
        format!("test -f {path} && exec tail -n 100 -F {path}")
    } else {
        format!("test -f {path} && cat {path}")
    };
    run_ssh(&record, false, &remote)
}

fn job_stop(args: &[OsString]) -> Result<i32> {
    let (requested, rest) = extract_mount_flag(args)?;
    if rest.len() != 1 {
        bail!("usage: cf job stop <job> [--mount <mount>]");
    }
    let id = rest[0].to_string_lossy().to_string();
    validate_job_id(&id)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let d = format!("$HOME/.cloudfolder/jobs/{id}");
    let remote = format!(
        "set -eu; d={d}; test -d \"$d\"; p=$(cat \"$d/pid\"); mode=$(cat \"$d/mode\" 2>/dev/null || echo plain); if kill -0 \"$p\" 2>/dev/null; then if [ \"$mode\" = setsid ]; then kill -TERM -- -\"$p\" 2>/dev/null || kill -TERM \"$p\"; else kill -TERM \"$p\"; fi; fi; printf 'stopped\\n' > \"$d/state\"; printf 'stopped %s\\n' {}",
        quote_posix(&id)
    );
    run_ssh(&record, false, &remote)
}

fn new_job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mixed = nanos ^ ((std::process::id() as u64) << 16);
    format!("cf-{:08x}", mixed as u32)
}

fn validate_job_id(id: &str) -> Result<()> {
    if id.starts_with("cf-")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        Ok(())
    } else {
        bail!("invalid CloudFolder job id")
    }
}

fn native_forward(args: &[OsString]) -> Result<i32> {
    let first = args.first().and_then(|arg| arg.to_str()).unwrap_or("list");
    if first.eq_ignore_ascii_case("list") {
        return forward_list(&args[1..]);
    }
    if first.eq_ignore_ascii_case("stop") {
        return forward_stop(&args[1..]);
    }
    forward_start(args)
}

fn forward_start(args: &[OsString]) -> Result<i32> {
    let (requested, rest) = extract_mount_flag(args)?;
    if rest.is_empty() || rest.len() > 2 {
        bail!("usage: cf forward <remote-port> [local-port] [--mount <mount>]");
    }
    let remote_port = parse_port(&rest[0], "remote port")?;
    let requested_local = if rest.len() == 2 {
        Some(parse_port(&rest[1], "local port")?)
    } else {
        None
    };
    let record = resolve_mount(requested.as_deref(), true)?;
    let local_port = choose_local_port(
        requested_local.unwrap_or(remote_port),
        requested_local.is_some(),
    )?;
    let mut command = ssh_command(&record)?;
    command
        .arg("-N")
        .arg("-L")
        .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg(ssh_target(&record));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .context("failed to start SSH port forward")?;
    thread::sleep(Duration::from_millis(600));
    if let Some(status) = child.try_wait().context("cannot inspect SSH forward")? {
        bail!("SSH port forward exited immediately with {status}");
    }
    let state = ForwardState {
        mount_slug: record.slug.clone(),
        remote_port,
        local_port,
        pid: child.id(),
        started_epoch: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    save_forward_state(&record, &state)?;
    println!(
        "Forward active: {} remote 127.0.0.1:{} -> 127.0.0.1:{} (pid {})",
        record.name, remote_port, local_port, state.pid
    );
    println!("Web URL (if applicable): http://127.0.0.1:{local_port}/");
    Ok(0)
}

fn forward_list(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let states = load_forward_states(&record)?;
    if states.is_empty() {
        println!("No saved forwards for {}.", record.name);
        return Ok(0);
    }
    println!("Local\tRemote\tPID\tState");
    for state in states {
        let active = forward_process_matches(&state);
        println!(
            "{}\t{}\t{}\t{}",
            state.local_port,
            state.remote_port,
            state.pid,
            if active { "running" } else { "stale" }
        );
    }
    Ok(0)
}

fn forward_stop(args: &[OsString]) -> Result<i32> {
    let (requested, rest) = extract_mount_flag(args)?;
    if rest.len() != 1 {
        bail!("usage: cf forward stop <local-port|all> [--mount <mount>]");
    }
    let record = resolve_mount(requested.as_deref(), true)?;
    let states = load_forward_states(&record)?;
    let all = rest[0].to_string_lossy().eq_ignore_ascii_case("all");
    let target_port = if all {
        None
    } else {
        Some(parse_port(&rest[0], "local port")?)
    };
    let mut matched = false;
    for state in states {
        if target_port.is_some_and(|port| port != state.local_port) {
            continue;
        }
        matched = true;
        if forward_process_matches(&state) {
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &state.pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_file(forward_state_path(&record, state.local_port));
        println!("Stopped forward localhost:{}.", state.local_port);
    }
    if !matched {
        bail!("no matching forward was found");
    }
    Ok(0)
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
    execute_remote_argv(&record, &cwd, &command)
}

fn native_sh(args: &[OsString]) -> Result<i32> {
    let (requested, command) = split_remote_command(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let shell_text = command
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    execute_remote_shell(&record, &cwd, &shell_text)
}

fn native_shell(args: &[OsString]) -> Result<i32> {
    let requested = single_optional_name(args)?;
    let record = resolve_mount(requested.as_deref(), true)?;
    let cwd = env::current_dir().context("cannot read the current directory")?;
    let remote_cwd = remote_working_directory(&record, &cwd)?;
    let environment = effective_environment(&record, &cwd)?;
    wait_for_flush(&record, Duration::from_secs(60))?;
    let mut body = format!("set -e\ncd -- {}\n", quote_posix(&remote_cwd));
    if !environment.init.trim().is_empty() {
        body.push_str(environment.init.trim_end());
        body.push('\n');
    }
    body.push_str("set +e\n");
    body.push_str("exec ${SHELL:-/bin/sh} -l");
    let remote_command = wrap_environment_shell(&environment, &body);
    let code = run_ssh(&record, true, &remote_command)?;
    let _ = refresh_vfs(&record);
    Ok(code)
}

fn execute_remote_argv(record: &MountRecord, cwd: &Path, command: &[OsString]) -> Result<i32> {
    if command.is_empty() {
        bail!("remote command is empty");
    }
    let remote_cwd = remote_working_directory(record, cwd)?;
    let environment = effective_environment(record, cwd)?;
    wait_for_flush(record, Duration::from_secs(60))?;
    let mut body = format!("set -e\ncd -- {}\n", quote_posix(&remote_cwd));
    if !environment.init.trim().is_empty() {
        body.push_str(environment.init.trim_end());
        body.push('\n');
    }
    body.push_str("set +e\n");
    body.push_str("exec");
    for arg in command {
        body.push(' ');
        body.push_str(&quote_posix(&arg.to_string_lossy()));
    }
    let remote_command = wrap_environment_shell(&environment, &body);
    let code = run_ssh(record, false, &remote_command)?;
    let _ = refresh_vfs(record);
    Ok(code)
}

fn execute_remote_shell(record: &MountRecord, cwd: &Path, shell_text: &str) -> Result<i32> {
    let remote_cwd = remote_working_directory(record, cwd)?;
    let environment = effective_environment(record, cwd)?;
    wait_for_flush(record, Duration::from_secs(60))?;
    let mut body = format!("set -e\ncd -- {}\n", quote_posix(&remote_cwd));
    if !environment.init.trim().is_empty() {
        body.push_str(environment.init.trim_end());
        body.push('\n');
    }
    body.push_str("set +e\n");
    body.push_str(shell_text);
    let remote_command = wrap_environment_shell(&environment, &body);
    let code = run_ssh(record, false, &remote_command)?;
    let _ = refresh_vfs(record);
    Ok(code)
}

fn launch_powershell(args: &[OsString]) -> Result<i32> {
    launch_powershell_named(args)
}

fn launch_powershell_named(args: &[OsString]) -> Result<i32> {
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

fn launch_manager_powershell(args: &[OsString]) -> Result<i32> {
    let script = manager_script()?;
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
        .context("failed to start CloudFolder manager")?;
    Ok(status.code().unwrap_or(1))
}

fn effective_environment(record: &MountRecord, cwd: &Path) -> Result<EffectiveEnvironment> {
    let Some(config_path) = find_workspace_config(record, cwd) else {
        return Ok(EffectiveEnvironment {
            config_path: None,
            shell: String::new(),
            init: String::new(),
            active: String::new(),
        });
    };
    let text = fs::read_to_string(&config_path)
        .with_context(|| format!("cannot read {}", config_path.display()))?;
    let config: WorkspaceConfig = toml::from_str(&text).with_context(|| {
        format!(
            "invalid CloudFolder workspace config {}",
            config_path.display()
        )
    })?;
    let mut shell = config.environment.shell.clone();
    let mut init = config.environment.init.clone();
    let active = environment_profile_override(record, &config_path)?
        .unwrap_or_else(|| config.environment.active.clone());
    if !active.trim().is_empty() {
        let profile = config.environment.profiles.get(&active).ok_or_else(|| {
            anyhow!(
                "environment profile '{}' is not defined in {}",
                active,
                config_path.display()
            )
        })?;
        if !profile.shell.trim().is_empty() {
            shell = profile.shell.clone();
        }
        if !profile.init.trim().is_empty() {
            if !init.trim().is_empty() {
                init.push('\n');
            }
            init.push_str(&profile.init);
        }
    }
    Ok(EffectiveEnvironment {
        config_path: Some(config_path),
        shell,
        init,
        active,
    })
}

fn find_workspace_config(record: &MountRecord, cwd: &Path) -> Option<PathBuf> {
    let root = PathBuf::from(&record.mount_point);
    let mut current = if relative_components(cwd, &root).is_some() {
        cwd.to_path_buf()
    } else {
        root.clone()
    };
    loop {
        let candidate = current.join(".cloudfolder.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if comparable_components(&current) == comparable_components(&root) {
            break;
        }
        if !current.pop() || relative_components(&current, &root).is_none() {
            break;
        }
    }
    None
}

fn environment_profile_state_path(record: &MountRecord) -> PathBuf {
    mount_data_dir(record).join("environment-profile")
}

fn environment_profile_override(
    record: &MountRecord,
    config_path: &Path,
) -> Result<Option<String>> {
    let path = environment_profile_state_path(record);
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut lines = text.lines();
    let stored_config = lines.next().unwrap_or_default();
    let profile = lines.next().unwrap_or_default();
    if stored_config.eq_ignore_ascii_case(&config_path.to_string_lossy())
        && !profile.trim().is_empty()
    {
        Ok(Some(profile.to_string()))
    } else {
        Ok(None)
    }
}

fn set_environment_profile(record: &MountRecord, cwd: &Path, profile: &str) -> Result<()> {
    let config_path = find_workspace_config(record, cwd).ok_or_else(|| {
        anyhow!(
            "no .cloudfolder.toml exists between the current directory and {}",
            record.mount_point
        )
    })?;
    let text = fs::read_to_string(&config_path)
        .with_context(|| format!("cannot read {}", config_path.display()))?;
    let config: WorkspaceConfig = toml::from_str(&text).with_context(|| {
        format!(
            "invalid CloudFolder workspace config {}",
            config_path.display()
        )
    })?;
    if !config.environment.profiles.contains_key(profile) {
        bail!(
            "environment profile '{}' is not defined in {}",
            profile,
            config_path.display()
        );
    }
    let state_path = environment_profile_state_path(record);
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(
        &state_path,
        format!("{}\n{}\n", config_path.display(), profile),
    )
    .with_context(|| format!("cannot write {}", state_path.display()))?;
    Ok(())
}

fn wrap_environment_shell(environment: &EffectiveEnvironment, body: &str) -> String {
    if environment.shell.trim().is_empty() {
        body.to_string()
    } else {
        format!("{} {}", environment.shell.trim(), quote_posix(body))
    }
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

fn manager_script() -> Result<PathBuf> {
    let exe = env::current_exe().context("cannot locate cf.exe")?;
    let parent = exe
        .parent()
        .ok_or_else(|| anyhow!("cannot locate the CloudFolder install directory"))?;
    let script = parent.join("CloudFolder.ps1");
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

fn mounts_dir() -> PathBuf {
    env::var_os("CLOUDFOLDER_MOUNTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(MOUNTS_DIR))
}

fn mount_data_dir(record: &MountRecord) -> PathBuf {
    if !record.rclone_config.trim().is_empty() {
        let path = PathBuf::from(&record.rclone_config);
        if let Some(parent) = path.parent() {
            return parent.to_path_buf();
        }
    }
    mounts_dir().join(&record.slug)
}

fn load_mounts() -> Result<Vec<MountRecord>> {
    let root = mounts_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("cannot read {}", root.display()))? {
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
    let (code, stdout, stderr) = run_ssh_capture(record, &command)?;
    if code != 0 {
        bail!(
            "could not resolve the remote root for '{}': {}",
            record.name,
            stderr.trim()
        );
    }
    let root = stdout.trim().to_string();
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
    command.arg(ssh_target(record)).arg(remote_command);
    let status = command
        .status()
        .context("failed to start Windows OpenSSH")?;
    Ok(status.code().unwrap_or(1))
}

fn run_ssh_capture(record: &MountRecord, remote_command: &str) -> Result<(i32, String, String)> {
    let output = ssh_command(record)?
        .arg(ssh_target(record))
        .arg(remote_command)
        .output()
        .context("failed to start Windows OpenSSH")?;
    Ok((
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn ssh_target(record: &MountRecord) -> String {
    if record.ssh_alias.trim().is_empty() {
        format!("{}@{}", record.user, record.host)
    } else {
        record.ssh_alias.clone()
    }
}

fn ssh_command(record: &MountRecord) -> Result<Command> {
    let mut command = Command::new("ssh.exe");
    if record.ssh_alias.trim().is_empty() {
        let (key_file, known_hosts) = ssh_files(record)?;
        command
            .arg("-p")
            .arg(record.port.to_string())
            .arg("-i")
            .arg(key_file)
            .args(["-o", "IdentitiesOnly=yes"])
            .args(["-o", "StrictHostKeyChecking=yes"])
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", known_hosts.display()));
    } else if !record.ssh_config.trim().is_empty() {
        command.arg("-F").arg(&record.ssh_config);
    }
    command
        .args(["-o", "BatchMode=yes"])
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
    let path = runtime_dir()?.join(name);
    if !path.is_file() {
        bail!("missing {}", path.display());
    }
    Ok(path)
}

fn runtime_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CLOUDFOLDER_RUNTIME_DIR") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return Ok(path);
        }
    }
    let exe = env::current_exe().context("cannot locate cf.exe")?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("cannot locate the CloudFolder install directory"))
}

fn extract_mount_flag(args: &[OsString]) -> Result<(Option<String>, Vec<OsString>)> {
    let mut requested = None;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new("--mount") {
            if requested.is_some() {
                bail!("--mount may be specified only once");
            }
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow!("--mount requires a mount name"))?;
            requested = Some(value.to_string_lossy().to_string());
            index += 2;
        } else {
            rest.push(args[index].clone());
            index += 1;
        }
    }
    Ok((requested, rest))
}

fn parse_port(value: &OsStr, label: &str) -> Result<u16> {
    let text = value.to_string_lossy();
    let port: u16 = text
        .parse()
        .with_context(|| format!("{label} must be a TCP port between 1 and 65535"))?;
    if port == 0 {
        bail!("{label} must be a TCP port between 1 and 65535");
    }
    Ok(port)
}

fn choose_local_port(preferred: u16, explicit: bool) -> Result<u16> {
    match TcpListener::bind(("127.0.0.1", preferred)) {
        Ok(listener) => {
            drop(listener);
            Ok(preferred)
        }
        Err(error) if explicit => bail!("local port {preferred} is unavailable: {error}"),
        Err(_) => {
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .context("could not allocate a free local forwarding port")?;
            let port = listener
                .local_addr()
                .context("could not inspect the allocated local forwarding port")?
                .port();
            drop(listener);
            Ok(port)
        }
    }
}

fn forward_state_dir(record: &MountRecord) -> PathBuf {
    mount_data_dir(record).join("forwards")
}

fn forward_state_path(record: &MountRecord, local_port: u16) -> PathBuf {
    forward_state_dir(record).join(format!("{local_port}.json"))
}

fn save_forward_state(record: &MountRecord, state: &ForwardState) -> Result<()> {
    let dir = forward_state_dir(record);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(state).context("cannot serialize forward state")?;
    let path = forward_state_path(record, state.local_port);
    fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

fn load_forward_states(record: &MountRecord) -> Result<Vec<ForwardState>> {
    let dir = forward_state_dir(record);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut states = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.path().extension() != Some(OsStr::new("json")) {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        let state: ForwardState = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid forward state {}", entry.path().display()))?;
        if state.mount_slug.eq_ignore_ascii_case(&record.slug) {
            states.push(state);
        }
    }
    states.sort_by_key(|state| state.local_port);
    Ok(states)
}

fn forward_process_matches(state: &ForwardState) -> bool {
    let script = format!(
        "$p=Get-CimInstance Win32_Process -Filter \"ProcessId = {}\" -ErrorAction SilentlyContinue; if($p){{$p.CommandLine}}",
        state.pid
    );
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-Command"])
        .arg(script)
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let command_line = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let forward = format!(
        "127.0.0.1:{}:127.0.0.1:{}",
        state.local_port, state.remote_port
    );
    command_line.contains("ssh") && command_line.contains(&forward)
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

    fn unique_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "cloudfolder-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn test_mount(root: &Path, data: &Path) -> MountRecord {
        MountRecord {
            name: "Test".to_string(),
            slug: "test".to_string(),
            service_name: "CloudFolder.test".to_string(),
            host: "example.com".to_string(),
            port: 22,
            user: "alice".to_string(),
            remote_path: "/workspace".to_string(),
            remote_root: "/workspace".to_string(),
            mount_point: root.to_string_lossy().to_string(),
            profile: "Dev".to_string(),
            rclone_config: data.join("rclone.conf").to_string_lossy().to_string(),
            key_file: String::new(),
            known_hosts: String::new(),
            ssh_alias: String::new(),
            ssh_config: String::new(),
            rc_port: 55770,
        }
    }

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
    fn router_only_claims_remote_runtime_tools() {
        assert_eq!(routed_tool_from_exe_name("git").as_deref(), Some("git"));
        assert_eq!(
            routed_tool_from_exe_name("PYTHON").as_deref(),
            Some("python")
        );
        assert!(routed_tool_from_exe_name("explorer").is_none());
        assert!(routed_tool_from_exe_name("code").is_none());
        assert!(routed_tool_from_exe_name("cf").is_none());
    }

    #[test]
    fn mount_flag_extraction_preserves_other_arguments() {
        let args = vec![
            OsString::from("8080"),
            OsString::from("--mount"),
            OsString::from("lab"),
            OsString::from("18080"),
        ];
        let (mount, rest) = extract_mount_flag(&args).unwrap();
        assert_eq!(mount.as_deref(), Some("lab"));
        assert_eq!(rest, vec![OsString::from("8080"), OsString::from("18080")]);
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
    fn workspace_environment_merges_profile_without_rewriting_config() {
        let root = unique_test_dir("env-root");
        let nested = root.join("project").join("src");
        let data = unique_test_dir("env-data");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&data).unwrap();
        let config_path = root.join(".cloudfolder.toml");
        fs::write(
            &config_path,
            r#"[environment]
shell = "bash -lc"
init = "export BASE=1"
active = "gpu"

[environment.profiles.gpu]
init = "export DEVICE=gpu"

[environment.profiles.cpu]
shell = "zsh -lc"
init = "export DEVICE=cpu"
"#,
        )
        .unwrap();
        let record = test_mount(&root, &data);
        let base = effective_environment(&record, &nested).unwrap();
        assert_eq!(base.shell, "bash -lc");
        assert_eq!(base.active, "gpu");
        assert!(base.init.contains("export BASE=1"));
        assert!(base.init.contains("export DEVICE=gpu"));

        set_environment_profile(&record, &nested, "cpu").unwrap();
        let selected = effective_environment(&record, &nested).unwrap();
        assert_eq!(selected.shell, "zsh -lc");
        assert_eq!(selected.active, "cpu");
        assert!(selected.init.contains("export DEVICE=cpu"));
        let original = fs::read_to_string(&config_path).unwrap();
        assert!(original.contains("active = \"gpu\""));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn environment_shell_wraps_one_remote_body() {
        let environment = EffectiveEnvironment {
            config_path: None,
            shell: "bash -lc".to_string(),
            init: String::new(),
            active: String::new(),
        };
        let wrapped = wrap_environment_shell(&environment, "echo 'hello'");
        assert!(wrapped.starts_with("bash -lc "));
        assert!(wrapped.contains("echo"));
        assert!(wrapped.contains("hello"));
    }

    #[test]
    fn forward_state_round_trips_per_mount() {
        let root = unique_test_dir("forward-root");
        let data = unique_test_dir("forward-data");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&data).unwrap();
        let record = test_mount(&root, &data);
        let state = ForwardState {
            mount_slug: "test".to_string(),
            remote_port: 8888,
            local_port: 18888,
            pid: 1234,
            started_epoch: 42,
        };
        save_forward_state(&record, &state).unwrap();
        let loaded = load_forward_states(&record).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].remote_port, 8888);
        assert_eq!(loaded[0].local_port, 18888);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn managed_agent_block_preserves_existing_content() {
        let original = "# My instructions\r\n\r\nKeep this.\r\n";
        let installed = upsert_managed_block(original).unwrap();
        assert!(installed.starts_with("# My instructions\r\n\r\nKeep this."));
        assert!(installed.contains(AGENT_BEGIN));
        assert!(installed.contains("cf enter"));
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
