use crate::config::Config;
use crate::logger::Logger;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::mem::size_of;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

struct KillOnCloseJob {
    handle: HANDLE,
}

impl KillOnCloseJob {
    fn new() -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("creating Windows Job Object");
        }

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(err).context("configuring Job Object KILL_ON_JOB_CLOSE");
        }
        Ok(Self { handle })
    }

    fn assign(&self, child: &Child) -> Result<()> {
        let process = child.as_raw_handle() as HANDLE;
        if unsafe { AssignProcessToJobObject(self.handle, process) } == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("assigning pid={} to Job Object", child.id()));
        }
        Ok(())
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

pub fn run(
    cfg: Config,
    config_path: Option<&Path>,
    logger: Logger,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    verify_dependencies(&cfg)?;
    let child_job = KillOnCloseJob::new()?;
    fs::create_dir_all(&cfg.mount.cache_dir)
        .with_context(|| format!("creating cache directory {}", cfg.mount.cache_dir.display()))?;
    if let Some(parent) = cfg.mount.mount_point.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating mount parent {}", parent.display()))?;
    }
    if let Some(parent) = cfg.logging.rclone_log.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut backoff = cfg.health.backoff_initial_secs;
    while !stop.load(Ordering::SeqCst) {
        cleanup_stale_mount(&cfg, &logger)?;
        logger.info(&format!(
            "starting rclone mount remote={} mount_point={}",
            cfg.mount.remote,
            cfg.mount.mount_point.display()
        ));

        match start_mount(&cfg, &child_job) {
            Ok(mut child) => {
                let pid = child.id();
                if !wait_until_ready(&cfg, &mut child, &stop, &logger)? {
                    logger.warn(&format!("mount pid={pid} did not become ready"));
                    stop_mount(&cfg, &mut child, &logger);
                    sleep_interruptible(backoff_with_jitter(backoff), &stop);
                    backoff = (backoff.saturating_mul(2)).min(cfg.health.backoff_max_secs);
                    continue;
                }

                logger.info(&format!("mount ready pid={pid}"));
                let mut change_feed = match config_path {
                    Some(path) => match start_change_feed(&cfg, path, &child_job) {
                        Ok(child) => child,
                        Err(err) => {
                            logger.warn(&format!("change feed did not start: {err:#}"));
                            None
                        }
                    },
                    None => None,
                };
                let stable_since = Instant::now();
                let reason = supervise_running_mount(
                    &cfg,
                    config_path,
                    &child_job,
                    &mut child,
                    &mut change_feed,
                    &stop,
                    &logger,
                )?;
                let stable_for = stable_since.elapsed().as_secs();
                logger.warn(&format!(
                    "mount ended/recycled pid={pid}: {reason}; stable_for={stable_for}s"
                ));
                stop_change_feed(&mut change_feed, &logger);
                stop_mount(&cfg, &mut child, &logger);
                let _ = cleanup_stale_mount(&cfg, &logger);

                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if stable_for >= cfg.health.stable_reset_secs {
                    backoff = cfg.health.backoff_initial_secs;
                } else {
                    backoff = (backoff.saturating_mul(2)).min(cfg.health.backoff_max_secs);
                }
                sleep_interruptible(backoff_with_jitter(backoff), &stop);
            }
            Err(err) => {
                logger.error(&format!("failed to start mount: {err:#}"));
                sleep_interruptible(backoff_with_jitter(backoff), &stop);
                backoff = (backoff.saturating_mul(2)).min(cfg.health.backoff_max_secs);
            }
        }
    }

    logger.info("service supervisor stopped");
    Ok(())
}

pub fn check_installation(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    cfg.validate()?;
    verify_dependencies(&cfg)?;
    run_remote_check(&cfg)?;
    println!("OK: configuration, dependencies, and SFTP connectivity are valid");
    Ok(())
}

pub fn check_remote(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    cfg.validate()?;
    if !cfg.mount.rclone_exe.is_file() {
        bail!(
            "rclone executable not found: {}",
            cfg.mount.rclone_exe.display()
        );
    }
    if !cfg.mount.rclone_config.is_file() {
        bail!(
            "rclone config not found: {}",
            cfg.mount.rclone_config.display()
        );
    }
    run_remote_check(&cfg)?;
    println!("OK: SFTP connectivity is valid");
    Ok(())
}

fn run_remote_check(cfg: &Config) -> Result<()> {
    let output = Command::new(&cfg.mount.rclone_exe)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("lsf")
        .arg(&cfg.mount.remote)
        .arg("--config")
        .arg(&cfg.mount.rclone_config)
        .arg("--max-depth")
        .arg("1")
        .arg("--contimeout")
        .arg("10s")
        .arg("--timeout")
        .arg("20s")
        .arg("--low-level-retries")
        .arg("1")
        .arg("--retries")
        .arg("1")
        .output()
        .context("running rclone connectivity check")?;
    if !output.status.success() {
        bail!(
            "rclone connectivity check failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn verify_dependencies(cfg: &Config) -> Result<()> {
    cfg.validate()?;
    if !cfg.mount.rclone_exe.is_file() {
        bail!(
            "rclone executable not found: {}",
            cfg.mount.rclone_exe.display()
        );
    }
    if !cfg.mount.rclone_config.is_file() {
        bail!(
            "rclone config not found: {}",
            cfg.mount.rclone_config.display()
        );
    }
    let winfsp = Path::new(r"C:\Program Files (x86)\WinFsp\bin\winfsp-x64.dll");
    if !winfsp.is_file() {
        bail!("WinFsp x64 runtime not found: {}", winfsp.display());
    }
    Ok(())
}

fn start_mount(cfg: &Config, child_job: &KillOnCloseJob) -> Result<Child> {
    let mut command = Command::new(&cfg.mount.rclone_exe);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("mount")
        .arg(&cfg.mount.remote)
        .arg(&cfg.mount.mount_point)
        .arg("--config")
        .arg(&cfg.mount.rclone_config)
        .arg("--volname")
        .arg(&cfg.mount.volume_name)
        .arg("--cache-dir")
        .arg(&cfg.mount.cache_dir)
        .arg("--vfs-cache-mode")
        .arg(&cfg.mount.vfs_cache_mode)
        .arg("--vfs-cache-max-size")
        .arg(&cfg.mount.vfs_cache_max_size)
        .arg("--vfs-cache-max-age")
        .arg(&cfg.mount.vfs_cache_max_age)
        .arg("--vfs-cache-min-free-space")
        .arg(&cfg.mount.vfs_cache_min_free_space)
        .arg("--vfs-write-back")
        .arg(&cfg.mount.vfs_write_back)
        .arg("--dir-cache-time")
        .arg(&cfg.mount.dir_cache_time)
        .arg("--attr-timeout")
        .arg(&cfg.mount.attr_timeout)
        .arg("--buffer-size")
        .arg(&cfg.mount.buffer_size)
        .arg("--vfs-read-ahead")
        .arg(&cfg.mount.vfs_read_ahead)
        .arg("--vfs-cache-poll-interval")
        .arg(&cfg.mount.vfs_cache_poll_interval)
        .arg("--transfers")
        .arg(cfg.mount.transfers.to_string())
        .arg("--file-perms")
        .arg(&cfg.mount.file_perms)
        .arg("--dir-perms")
        .arg(&cfg.mount.dir_perms)
        .arg("--contimeout")
        .arg("10s")
        .arg("--timeout")
        .arg("30s")
        .arg("--low-level-retries")
        .arg("3")
        .arg("--rc")
        .arg("--rc-addr")
        .arg(&cfg.mount.rc_addr)
        .arg("--rc-no-auth")
        .arg("--log-level")
        .arg("NOTICE")
        .arg("--log-file")
        .arg(&cfg.logging.rclone_log)
        .arg("--log-file-max-size")
        .arg(cfg.logging.max_bytes.to_string())
        .arg("--log-file-max-backups")
        .arg(cfg.logging.keep_files.max(1).to_string())
        .arg("--log-file-compress")
        .arg("--log-format")
        .arg("date,time,microseconds,pid")
        .arg("--no-console");
    if !cfg.mount.windows_file_security.trim().is_empty() {
        command
            .arg("-o")
            .arg(format!("FileSecurity={}", cfg.mount.windows_file_security));
    }
    if cfg.mount.read_only {
        command.arg("--read-only");
    }
    let mut child = command.spawn().context("spawning rclone mount")?;
    if let Err(err) = child_job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    Ok(child)
}

fn wait_until_ready(
    cfg: &Config,
    child: &mut Child,
    stop: &Arc<AtomicBool>,
    logger: &Logger,
) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(cfg.health.startup_timeout_secs);
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        if let Some(status) = child.try_wait()? {
            logger.error(&format!("rclone exited during startup: {status}"));
            return Ok(false);
        }
        if probe_mount(
            &cfg.mount.mount_point,
            Duration::from_secs(cfg.health.probe_timeout_secs),
        ) {
            return Ok(true);
        }
        sleep_interruptible(1, stop);
    }
    Ok(false)
}

fn supervise_running_mount(
    cfg: &Config,
    config_path: Option<&Path>,
    child_job: &KillOnCloseJob,
    child: &mut Child,
    change_feed: &mut Option<Child>,
    stop: &Arc<AtomicBool>,
    logger: &Logger,
) -> Result<String> {
    let mut failures = 0u32;
    let mut next_probe = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok("service stop requested".into());
        }
        if let Some(status) = child.try_wait()? {
            return Ok(format!("rclone exited with {status}"));
        }

        if cfg.change_feed.enabled {
            let should_restart = match change_feed.as_mut() {
                Some(feed) => match feed.try_wait()? {
                    Some(status) => {
                        logger.warn(&format!("change feed exited with {status}; restarting"));
                        true
                    }
                    None => false,
                },
                None => true,
            };
            if should_restart {
                *change_feed = match config_path {
                    Some(path) => match start_change_feed(cfg, path, child_job) {
                        Ok(child) => child,
                        Err(err) => {
                            logger.warn(&format!("change feed restart failed: {err:#}"));
                            None
                        }
                    },
                    None => None,
                };
            }
        }

        if Instant::now() >= next_probe {
            if probe_mount(
                &cfg.mount.mount_point,
                Duration::from_secs(cfg.health.probe_timeout_secs),
            ) {
                if failures > 0 {
                    logger.info(&format!(
                        "mount health recovered after {failures} failed probe(s)"
                    ));
                }
                failures = 0;
            } else {
                failures += 1;
                logger.warn(&format!(
                    "mount health probe failed ({failures}/{})",
                    cfg.health.failure_threshold
                ));
                if failures >= cfg.health.failure_threshold {
                    return Ok(format!("health failed {failures} consecutive probes"));
                }
            }
            next_probe = Instant::now() + cfg.probe_interval();
        }
        // Poll the child frequently for fast crash recovery, but run the
        // potentially blocking filesystem probe at its own lower cadence.
        sleep_interruptible(1, stop);
    }
}

fn start_change_feed(
    cfg: &Config,
    config_path: &Path,
    child_job: &KillOnCloseJob,
) -> Result<Option<Child>> {
    if !cfg.change_feed.enabled {
        return Ok(None);
    }
    let exe = std::env::current_exe().context("locating CloudFolder service executable")?;
    let mut child = Command::new(exe)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("change-feed")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning change-feed worker")?;
    if let Err(err) = child_job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    Ok(Some(child))
}

fn stop_change_feed(change_feed: &mut Option<Child>, logger: &Logger) {
    let Some(mut child) = change_feed.take() else {
        return;
    };
    if child.try_wait().ok().flatten().is_none() {
        logger.info(&format!("stopping change feed pid={}", child.id()));
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub fn run_change_feed(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    cfg.validate()?;
    if !cfg.change_feed.enabled {
        return Ok(());
    }
    if !cfg.change_feed.ssh_exe.is_file() {
        bail!(
            "change-feed ssh executable not found: {}",
            cfg.change_feed.ssh_exe.display()
        );
    }
    if !cfg.change_feed.ssh_config.is_file() {
        bail!(
            "change-feed SSH config not found: {}",
            cfg.change_feed.ssh_config.display()
        );
    }
    let logger = Logger::new(
        &cfg.logging.service_log,
        cfg.logging.max_bytes,
        cfg.logging.keep_files,
    )?;
    let helper = remote_change_feed_script(
        cfg.change_feed.debounce_ms,
        cfg.change_feed.max_watches,
        cfg.change_feed.reserve_watches,
    );
    let remote = format!(
        "exec python3 -u -c {} {}",
        posix_quote(&helper),
        posix_quote(&cfg.change_feed.remote_root)
    );
    loop {
        let mut ssh = Command::new(&cfg.change_feed.ssh_exe);
        ssh.creation_flags(CREATE_NO_WINDOW)
            .arg("-F")
            .arg(&cfg.change_feed.ssh_config)
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(["-o", "ServerAliveCountMax=3"])
            .arg(&cfg.change_feed.ssh_target)
            .arg(&remote)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = ssh
            .spawn()
            .context("starting remote change-feed SSH session")?;
        logger.info(&format!(
            "change feed connected target={} root={} pid={}",
            cfg.change_feed.ssh_target,
            cfg.change_feed.remote_root,
            child.id()
        ));
        let stdout = child
            .stdout
            .take()
            .context("change-feed stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        while reader.read_line(&mut line)? != 0 {
            let text = line.trim_end_matches(['\r', '\n']);
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(ready) = value.get("ready_dirs").and_then(|v| v.as_u64()) {
                    let scanned = value
                        .get("scanned_dirs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let projects = value
                        .get("project_roots")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let failures = value
                        .get("watch_failures")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let limit = value
                        .get("watch_limit")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let degraded = value
                        .get("degraded")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    logger.info(&format!(
                        "change feed ready; watched_dirs={ready} scanned_dirs={scanned} project_roots={projects} watch_limit={limit} watch_failures={failures} degraded={degraded}"
                    ));
                } else if value.get("overflow").and_then(|v| v.as_bool()) == Some(true) {
                    logger.warn(
                        "change feed queue overflow; invalidating the full VFS directory cache",
                    );
                    let _ = rc_command(&cfg, "vfs/forget").status();
                } else {
                    let file = value.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    let dir = value.get("dir").and_then(|v| v.as_str()).unwrap_or("");
                    if (!file.is_empty() || !dir.is_empty())
                        && targeted_forget(&cfg, file, dir).is_err()
                    {
                        logger.warn(&format!(
                            "targeted VFS invalidation failed for file='{file}' dir='{dir}'"
                        ));
                    }
                }
            }
            line.clear();
        }
        let status = child.wait().context("waiting for change-feed SSH")?;
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
        logger.warn(&format!(
            "change feed disconnected ({status}); {}",
            stderr.trim()
        ));
        thread::sleep(Duration::from_secs(1));
    }
}

fn targeted_forget(cfg: &Config, file: &str, dir: &str) -> Result<()> {
    let mut command = rc_command(cfg, "vfs/forget");
    if !file.is_empty() {
        command.arg(format!("file={file}"));
    }
    if !dir.is_empty() {
        command.arg(format!("dir={dir}"));
    }
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running targeted vfs/forget")?;
    if !output.status.success() {
        bail!(
            "vfs/forget failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_change_feed_script(debounce_ms: u64, configured_max: u64, reserve: u64) -> String {
    let debounce = (debounce_ms as f64 / 1000.0).clamp(0.025, 5.0);
    format!(
        r#"import ctypes,ctypes.util,json,os,select,struct,sys,time
root=os.path.realpath(sys.argv[1])
libc=ctypes.CDLL(ctypes.util.find_library('c') or 'libc.so.6',use_errno=True)
fd=libc.inotify_init1(0)
if fd<0: raise OSError(ctypes.get_errno(),'inotify_init1')
MASK=0x00000002|0x00000004|0x00000008|0x00000040|0x00000080|0x00000100|0x00000200|0x00000400|0x00000800
ISDIR=0x40000000
OVERFLOW=0x00004000
try:
    kernel_limit=int(open('/proc/sys/fs/inotify/max_user_watches').read().strip())
except Exception:
    kernel_limit={configured_max}
limit=max(1024,min({configured_max},max(1024,kernel_limit//4)))
reserve=min({reserve},max(0,limit//4))
initial_limit=max(1,limit-reserve)
wd_to_path={{}}
path_to_wd={{}}
watch_failures=0
watch_exhausted=False
def add_dir(p):
    global watch_failures,watch_exhausted
    p=os.path.realpath(p)
    if p in path_to_wd: return True
    if watch_exhausted or len(path_to_wd)>=limit: return False
    wd=libc.inotify_add_watch(fd,os.fsencode(p),MASK)
    if wd>=0:
        old=wd_to_path.get(wd)
        if old: path_to_wd.pop(old,None)
        wd_to_path[wd]=p; path_to_wd[p]=wd; return True
    watch_failures+=1
    if ctypes.get_errno()==28: watch_exhausted=True
    return False
def add_tree(p,budget):
    before=len(path_to_wd)
    for base,dirs,files in os.walk(p,followlinks=False):
        if watch_exhausted or len(path_to_wd)>=budget: break
        add_dir(base)
    return len(path_to_wd)-before
PROJECT_MARKERS={{'.git','.cloudfolder.toml','pyproject.toml','Cargo.toml','package.json','go.mod'}}
DISCOVERY_SKIP={{'.cache','.npm','.rustup','.local','.conda','node_modules','target','__pycache__','.venv','venv'}}
DISCOVERY_DEPTH=5
add_dir(root)
projects=[]; scanned=0
for base,dirs,files in os.walk(root,topdown=True,followlinks=False):
    scanned+=1
    rel=os.path.relpath(base,root); depth=0 if rel=='.' else rel.count(os.sep)+1
    names=set(dirs)|set(files)
    is_project=bool(PROJECT_MARKERS & names)
    if depth<=2: add_dir(base)
    if is_project:
        projects.append(base); dirs[:]=[]; continue
    dirs[:]=[d for d in dirs if d not in DISCOVERY_SKIP]
    if depth>=DISCOVERY_DEPTH: dirs[:]=[]
for p in sorted(projects,key=len):
    if watch_exhausted or len(path_to_wd)>=initial_limit: break
    add_tree(p,initial_limit)
degraded=watch_failures>0 or len(path_to_wd)>=initial_limit or (scanned>len(path_to_wd) and not projects)
print(json.dumps({{'ready_dirs':len(path_to_wd),'scanned_dirs':scanned,'project_roots':len(projects),'watch_limit':limit,'watch_failures':watch_failures,'degraded':degraded}}),flush=True)
pending={{}}
last=time.monotonic()
while True:
    ready,_,_=select.select([fd],[],[],{debounce})
    if ready:
        data=os.read(fd,1024*1024); off=0
        while off+16<=len(data):
            wd,mask,cookie,nlen=struct.unpack_from('iIII',data,off); off+=16
            raw=data[off:off+nlen]; off+=nlen
            name=os.fsdecode(raw.split(b'\0',1)[0]) if nlen else ''
            base=wd_to_path.get(wd,root); full=os.path.join(base,name) if name else base
            if mask & OVERFLOW:
                print(json.dumps({{'overflow':True}}),flush=True); continue
            new_dir=bool(mask & ISDIR and mask & (0x00000100|0x00000080) and os.path.isdir(full))
            if new_dir: add_tree(full,limit)
            if mask & (0x00000400|0x00000800):
                old=wd_to_path.pop(wd,None)
                if old: path_to_wd.pop(old,None)
            rel=os.path.relpath(full,root)
            if rel.startswith('..'): continue
            parent=os.path.dirname(rel) or '.'
            pending[(rel,parent)]=1
            if new_dir: pending[('',rel)]=1
        last=time.monotonic()
    elif pending and time.monotonic()-last>={debounce}:
        if len(pending)<=512:
            for rel,parent in pending:
                print(json.dumps({{'file':rel.replace(os.sep,'/'),'dir':parent.replace(os.sep,'/')}}),flush=True)
        else:
            dirs=sorted({{parent for rel,parent in pending}})
            for parent in dirs:
                print(json.dumps({{'file':'','dir':parent.replace(os.sep,'/'),'bulk_events':len(pending)}}),flush=True)
        pending.clear()
"#
    )
}

fn probe_mount(mount_point: &Path, timeout: Duration) -> bool {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return false,
    };
    process_succeeds_with_timeout(
        Command::new(exe)
            .creation_flags(CREATE_NO_WINDOW)
            .arg("probe-path")
            .arg(mount_point),
        timeout,
    )
}

pub fn probe_path_once(path: &Path) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("probing mount directory {}", path.display()))?;
    if let Some(entry) = entries.next() {
        entry.with_context(|| format!("reading first entry from {}", path.display()))?;
    }
    Ok(())
}

fn process_succeeds_with_timeout(command: &mut Command, timeout: Duration) -> bool {
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn stop_mount(cfg: &Config, child: &mut Child, logger: &Logger) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let child_pid = child.id();
    match rc_pid(cfg) {
        Some(pid) if pid == child_pid => {
            logger.info(&format!(
                "requesting graceful rclone shutdown pid={child_pid}"
            ));
            let mut command = rc_command(cfg, "core/quit");
            if !process_succeeds_with_timeout(&mut command, Duration::from_secs(3)) {
                logger.warn(&format!(
                    "RC core/quit failed or timed out for pid={child_pid}"
                ));
            }
        }
        Some(pid) => logger.warn(&format!(
            "RC endpoint PID mismatch: expected={child_pid} actual={pid}; refusing core/quit"
        )),
        None => logger.warn(&format!(
            "RC endpoint unavailable for pid={child_pid}; falling back to process termination if needed"
        )),
    }

    let deadline = Instant::now() + Duration::from_secs(cfg.health.graceful_stop_secs);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => thread::sleep(Duration::from_millis(200)),
        }
    }
    logger.warn(&format!(
        "graceful shutdown timed out; killing pid={}",
        child.id()
    ));
    let _ = child.kill();
    let _ = child.wait();
}

fn rc_command(cfg: &Config, method: &str) -> Command {
    let mut command = Command::new(&cfg.mount.rclone_exe);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .arg("rc")
        .arg("--url")
        .arg(format!("http://{}/", cfg.mount.rc_addr))
        .arg(method)
        .stdin(Stdio::null());
    command
}

fn rc_pid(cfg: &Config) -> Option<u32> {
    let mut command = rc_command(cfg, "core/pid");
    let (status, output) = process_stdout_with_timeout(&mut command, Duration::from_secs(3))?;
    if !status.success() {
        return None;
    }
    parse_rc_pid(&output)
}

fn parse_rc_pid(output: &str) -> Option<u32> {
    let key = output.find("\"pid\"")?;
    let after_key = &output[key + 5..];
    let colon = after_key.find(':')?;
    after_key[colon + 1..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

fn process_stdout_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Option<(ExitStatus, String)> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut text = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut text);
                }
                return Some((status, text));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub fn cleanup_stale_mount(cfg: &Config, logger: &Logger) -> Result<()> {
    let path = &cfg.mount.mount_point;
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("stat mount point {}", path.display()))
        }
    };

    let is_reparse = (metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
    if is_reparse {
        logger.warn(&format!(
            "stale reparse mount point detected: {}",
            path.display()
        ));
        let _ = Command::new("mountvol.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .arg(path)
            .arg("/D")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if fs::symlink_metadata(path).is_ok() {
            let _ = fs::remove_dir(path);
        }
        if fs::symlink_metadata(path).is_ok() {
            bail!(
                "unable to remove stale reparse mount point {}",
                path.display()
            );
        }
        return Ok(());
    }

    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("reading existing mount directory {}", path.display()))?;
        if entries.next().is_none() {
            logger.warn(&format!(
                "removing empty leftover mount directory {}",
                path.display()
            ));
            fs::remove_dir(path)?;
            return Ok(());
        }
        bail!(
            "mount point {} is a non-empty normal directory; refusing to hide or delete user data",
            path.display()
        );
    }
    bail!(
        "mount point {} exists and is not a directory",
        path.display()
    )
}

fn sleep_interruptible(seconds: u64, stop: &Arc<AtomicBool>) {
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
    }
}

fn backoff_with_jitter(base: u64) -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis() as u64;
    base.saturating_add((millis % 1000) / 250).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn unique_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "cloudfolder-service-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn cleanup_test_config(mount_point: &Path) -> Config {
        let text = format!(
            r#"
[mount]
rclone_exe = 'C:\rclone.exe'
rclone_config = 'C:\rclone.conf'
remote = 'test:/'
mount_point = '{}'
cache_dir = 'C:\CloudFolderCacheTest'
"#,
            mount_point.display()
        );
        toml::from_str(&text).unwrap()
    }

    #[test]
    fn jitter_is_bounded() {
        let value = backoff_with_jitter(10);
        assert!((10..=13).contains(&value));
    }

    #[test]
    fn change_feed_event_loop_is_event_driven_and_bulk_coalesces() {
        let script = remote_change_feed_script(150, 60_000, 4_096);
        let event_loop = script
            .split("while True:")
            .nth(1)
            .expect("change-feed helper must have an event loop");
        assert!(event_loop.contains("select.select"));
        assert!(!event_loop.contains("os.walk("));
        assert!(event_loop.contains("if len(pending)<=512"));
        assert!(event_loop.contains("bulk_events"));
        assert!(script.contains("kernel_limit//4"));
        assert!(script.contains("watch_exhausted"));
    }

    #[test]
    fn normal_file_is_not_reparse() {
        let path = PathBuf::from("Cargo.toml");
        let meta = fs::symlink_metadata(path).unwrap();
        assert_eq!(meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
    }

    #[test]
    fn parses_rc_pid() {
        assert_eq!(parse_rc_pid("{\n  \"pid\": 19264\n}"), Some(19264));
        assert_eq!(parse_rc_pid("{}"), None);
        assert_eq!(parse_rc_pid("{\"pid\":\"oops\"}"), None);
    }

    #[test]
    fn job_object_kills_assigned_child_on_close() {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let job = KillOnCloseJob::new().unwrap();
        job.assign(&child).unwrap();
        drop(job);

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut exited = false;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            let _ = child.kill();
        }
        let _ = child.wait();
        assert!(
            exited,
            "assigned child survived closing KILL_ON_JOB_CLOSE job"
        );
    }

    #[test]
    fn timed_process_is_terminated() {
        let started = Instant::now();
        let ok = process_succeeds_with_timeout(
            Command::new("powershell.exe").args([
                "-NoProfile",
                "-Command",
                "Start-Sleep -Seconds 10",
            ]),
            Duration::from_millis(200),
        );
        assert!(!ok);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn cleanup_removes_only_empty_normal_mountpoint() {
        let root = unique_test_dir("cleanup-empty");
        let mount = root.join("mount");
        fs::create_dir_all(&mount).unwrap();
        let cfg = cleanup_test_config(&mount);
        let logger = Logger::new(&root.join("test.log"), 1024 * 1024, 1).unwrap();

        cleanup_stale_mount(&cfg, &logger).unwrap();
        assert!(!mount.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_refuses_nonempty_normal_mountpoint() {
        let root = unique_test_dir("cleanup-nonempty");
        let mount = root.join("mount");
        fs::create_dir_all(&mount).unwrap();
        let sentinel = mount.join("do-not-delete.txt");
        fs::write(&sentinel, b"sentinel").unwrap();
        let cfg = cleanup_test_config(&mount);
        let logger = Logger::new(&root.join("test.log"), 1024 * 1024, 1).unwrap();

        assert!(cleanup_stale_mount(&cfg, &logger).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn probe_path_once_accepts_empty_and_nonempty_directories() {
        let root = unique_test_dir("probe-path");
        fs::create_dir_all(&root).unwrap();
        probe_path_once(&root).unwrap();
        fs::write(root.join("entry.txt"), b"ok").unwrap();
        probe_path_once(&root).unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
