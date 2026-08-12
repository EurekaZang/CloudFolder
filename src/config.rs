use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mount: MountConfig,
    #[serde(default)]
    pub change_feed: ChangeFeedConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangeFeedConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_ssh_exe")]
    pub ssh_exe: PathBuf,
    #[serde(default)]
    pub ssh_config: PathBuf,
    #[serde(default)]
    pub ssh_target: String,
    #[serde(default)]
    pub remote_root: String,
    #[serde(default = "default_change_feed_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_change_feed_max_watches")]
    pub max_watches: u64,
    #[serde(default = "default_change_feed_reserve_watches")]
    pub reserve_watches: u64,
}

impl Default for ChangeFeedConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ssh_exe: default_ssh_exe(),
            ssh_config: PathBuf::new(),
            ssh_target: String::new(),
            remote_root: String::new(),
            debounce_ms: default_change_feed_debounce_ms(),
            max_watches: default_change_feed_max_watches(),
            reserve_watches: default_change_feed_reserve_watches(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MountConfig {
    pub rclone_exe: PathBuf,
    pub rclone_config: PathBuf,
    pub remote: String,
    pub mount_point: PathBuf,
    pub cache_dir: PathBuf,
    #[serde(default = "default_volume_name")]
    pub volume_name: String,
    #[serde(default = "default_cache_mode")]
    pub vfs_cache_mode: String,
    #[serde(default = "default_cache_max_size")]
    pub vfs_cache_max_size: String,
    #[serde(default = "default_cache_max_age")]
    pub vfs_cache_max_age: String,
    #[serde(default = "default_cache_min_free_space")]
    pub vfs_cache_min_free_space: String,
    #[serde(default = "default_write_back")]
    pub vfs_write_back: String,
    #[serde(default = "default_dir_cache_time")]
    pub dir_cache_time: String,
    #[serde(default = "default_attr_timeout")]
    pub attr_timeout: String,
    #[serde(default = "default_buffer_size")]
    pub buffer_size: String,
    #[serde(default = "default_read_ahead")]
    pub vfs_read_ahead: String,
    #[serde(default = "default_cache_poll_interval")]
    pub vfs_cache_poll_interval: String,
    #[serde(default = "default_transfers")]
    pub transfers: u32,
    #[serde(default = "default_file_perms")]
    pub file_perms: String,
    #[serde(default = "default_dir_perms")]
    pub dir_perms: String,
    #[serde(default)]
    pub windows_file_security: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_rc_addr")]
    pub rc_addr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    #[serde(default = "default_probe_interval_secs")]
    pub probe_interval_secs: u64,
    #[serde(default = "default_probe_timeout_secs")]
    pub probe_timeout_secs: u64,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(default = "default_backoff_initial_secs")]
    pub backoff_initial_secs: u64,
    #[serde(default = "default_backoff_max_secs")]
    pub backoff_max_secs: u64,
    #[serde(default = "default_stable_reset_secs")]
    pub stable_reset_secs: u64,
    #[serde(default = "default_graceful_stop_secs")]
    pub graceful_stop_secs: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            probe_interval_secs: default_probe_interval_secs(),
            probe_timeout_secs: default_probe_timeout_secs(),
            startup_timeout_secs: default_startup_timeout_secs(),
            failure_threshold: default_failure_threshold(),
            backoff_initial_secs: default_backoff_initial_secs(),
            backoff_max_secs: default_backoff_max_secs(),
            stable_reset_secs: default_stable_reset_secs(),
            graceful_stop_secs: default_graceful_stop_secs(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_service_log")]
    pub service_log: PathBuf,
    #[serde(default = "default_rclone_log")]
    pub rclone_log: PathBuf,
    #[serde(default = "default_log_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_keep_files")]
    pub keep_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            service_log: default_service_log(),
            rclone_log: default_rclone_log(),
            max_bytes: default_log_max_bytes(),
            keep_files: default_keep_files(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.mount.remote.trim().is_empty() || !self.mount.remote.contains(':') {
            bail!("mount.remote must be a valid rclone remote such as remote:/data");
        }
        if !self.mount.mount_point.is_absolute() {
            bail!("mount.mount_point must be absolute");
        }
        if !self.mount.cache_dir.is_absolute() {
            bail!("mount.cache_dir must be absolute");
        }
        if self.health.probe_interval_secs == 0 || self.health.probe_timeout_secs == 0 {
            bail!("health probe interval/timeout must be > 0");
        }
        if self.health.failure_threshold == 0 {
            bail!("health.failure_threshold must be > 0");
        }
        if self.health.backoff_initial_secs == 0
            || self.health.backoff_initial_secs > self.health.backoff_max_secs
        {
            bail!("health backoff values are invalid");
        }
        let rc_addr = self
            .mount
            .rc_addr
            .parse::<std::net::SocketAddr>()
            .map_err(|_| anyhow::anyhow!("mount.rc_addr must be an IP:port socket address"))?;
        if !rc_addr.ip().is_loopback() {
            bail!("mount.rc_addr must use a loopback address because RC is configured without authentication");
        }
        if self.mount.mount_point == self.mount.cache_dir {
            bail!("mount.mount_point and mount.cache_dir must be different paths");
        }
        if self.mount.transfers == 0 {
            bail!("mount.transfers must be > 0");
        }
        for (label, value) in [
            ("mount.file_perms", self.mount.file_perms.as_str()),
            ("mount.dir_perms", self.mount.dir_perms.as_str()),
        ] {
            if value.len() != 4
                || !value.starts_with('0')
                || !value.chars().all(|c| c.is_ascii_digit() && c < '8')
            {
                bail!("{label} must be an octal mode such as 0666 or 0777");
            }
        }
        if self.mount.windows_file_security.contains('\r')
            || self.mount.windows_file_security.contains('\n')
        {
            bail!("mount.windows_file_security cannot contain line breaks");
        }
        if self.change_feed.enabled {
            if !self.change_feed.ssh_exe.is_absolute() {
                bail!("change_feed.ssh_exe must be absolute when change feed is enabled");
            }
            if !self.change_feed.ssh_config.is_absolute() {
                bail!("change_feed.ssh_config must be absolute when change feed is enabled");
            }
            if self.change_feed.ssh_target.trim().is_empty() {
                bail!("change_feed.ssh_target is required when change feed is enabled");
            }
            if !self.change_feed.remote_root.starts_with('/') {
                bail!("change_feed.remote_root must be an absolute Linux path");
            }
            if self.change_feed.debounce_ms < 25 || self.change_feed.debounce_ms > 5000 {
                bail!("change_feed.debounce_ms must be between 25 and 5000");
            }
            if self.change_feed.max_watches < 1024
                || self.change_feed.reserve_watches >= self.change_feed.max_watches
            {
                bail!("change_feed watch budget is invalid");
            }
        }
        Ok(())
    }

    pub fn probe_interval(&self) -> Duration {
        Duration::from_secs(self.health.probe_interval_secs)
    }
}

fn default_volume_name() -> String {
    "CloudFolder".into()
}
fn default_cache_mode() -> String {
    "full".into()
}
fn default_cache_max_size() -> String {
    "32Gi".into()
}
fn default_cache_max_age() -> String {
    "168h".into()
}
fn default_cache_min_free_space() -> String {
    "16Gi".into()
}
fn default_write_back() -> String {
    "5s".into()
}
fn default_dir_cache_time() -> String {
    "30s".into()
}
fn default_attr_timeout() -> String {
    "1s".into()
}
fn default_buffer_size() -> String {
    "16Mi".into()
}
fn default_read_ahead() -> String {
    "64Mi".into()
}
fn default_cache_poll_interval() -> String {
    "30s".into()
}
fn default_transfers() -> u32 {
    4
}
fn default_file_perms() -> String {
    "0666".into()
}
fn default_dir_perms() -> String {
    "0777".into()
}
fn default_rc_addr() -> String {
    "127.0.0.1:5577".into()
}
fn default_ssh_exe() -> PathBuf {
    PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe")
}
fn default_change_feed_debounce_ms() -> u64 {
    150
}
fn default_change_feed_max_watches() -> u64 {
    60_000
}
fn default_change_feed_reserve_watches() -> u64 {
    4_096
}
fn default_probe_interval_secs() -> u64 {
    10
}
fn default_probe_timeout_secs() -> u64 {
    5
}
fn default_startup_timeout_secs() -> u64 {
    60
}
fn default_failure_threshold() -> u32 {
    3
}
fn default_backoff_initial_secs() -> u64 {
    1
}
fn default_backoff_max_secs() -> u64 {
    60
}
fn default_stable_reset_secs() -> u64 {
    180
}
fn default_graceful_stop_secs() -> u64 {
    15
}
fn default_service_log() -> PathBuf {
    PathBuf::from(r"C:\ProgramData\CloudFolder\logs\service.log")
}
fn default_rclone_log() -> PathBuf {
    PathBuf::from(r"C:\ProgramData\CloudFolder\logs\rclone.log")
}
fn default_log_max_bytes() -> u64 {
    20 * 1024 * 1024
}
fn default_keep_files() -> usize {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let health = HealthConfig::default();
        assert!(health.probe_timeout_secs < health.startup_timeout_secs);
        assert!(health.backoff_initial_secs <= health.backoff_max_secs);
        assert!(health.failure_threshold >= 1);
        let feed = ChangeFeedConfig::default();
        assert!(feed.debounce_ms > 0);
        assert!(feed.reserve_watches < feed.max_watches);
        assert!(feed.max_watches >= 1024);
    }

    #[test]
    fn rc_must_be_loopback() {
        let text = r#"
[mount]
rclone_exe = "C:\\rclone.exe"
rclone_config = "C:\\rclone.conf"
remote = "x:/"
mount_point = "C:\\CloudFolder\\Example"
cache_dir = "C:\\.CloudFolderCache\\Example"
rc_addr = "0.0.0.0:5577"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn developer_mount_options_validate() {
        let text = r#"
[mount]
rclone_exe = "C:\\rclone.exe"
rclone_config = "C:\\rclone.conf"
remote = "x:/"
mount_point = "C:\\CloudFolder\\Example"
cache_dir = "C:\\.CloudFolderCache\\Example"
vfs_cache_mode = "writes"
vfs_cache_poll_interval = "2s"
transfers = 8
file_perms = "0777"
dir_perms = "0777"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_ok());
    }
}
