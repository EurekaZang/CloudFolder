use crate::config::Config;
use crate::logger::Logger;
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Read;
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

pub fn run(cfg: Config, logger: Logger, stop: Arc<AtomicBool>) -> Result<()> {
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
                let stable_since = Instant::now();
                let reason = supervise_running_mount(&cfg, &mut child, &stop, &logger)?;
                let stable_for = stable_since.elapsed().as_secs();
                logger.warn(&format!(
                    "mount ended/recycled pid={pid}: {reason}; stable_for={stable_for}s"
                ));
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
    child: &mut Child,
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
