mod config;
mod logger;
mod supervisor;

use anyhow::{Context, Result};
use config::Config;
use logger::Logger;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

const SERVICE_NAME: &str = "CloudFolder.Default";
const DEFAULT_CONFIG: &str = r"C:\ProgramData\CloudFolder\mounts\default\service.toml";

#[derive(Debug)]
struct ServiceRuntime {
    name: String,
    config_path: PathBuf,
}

static SERVICE_RUNTIME: OnceLock<ServiceRuntime> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("console") => {
            let config_path = args.next().map(PathBuf::from).unwrap_or_else(default_config_path);
            run_console(&config_path)
        }
        Some("check") => {
            let config_path = args.next().map(PathBuf::from).unwrap_or_else(default_config_path);
            supervisor::check_installation(&config_path)
        }
        Some("check-remote") => {
            let config_path = args.next().map(PathBuf::from).unwrap_or_else(default_config_path);
            supervisor::check_remote(&config_path)
        }
        Some("cleanup") => {
            let config_path = args.next().map(PathBuf::from).unwrap_or_else(default_config_path);
            let cfg = Config::load(&config_path)?;
            let logger = Logger::new(&cfg.logging.service_log, cfg.logging.max_bytes, cfg.logging.keep_files)?;
            supervisor::cleanup_stale_mount(&cfg, &logger)
        }
        Some("change-feed") => {
            let config_path = args
                .next()
                .map(PathBuf::from)
                .context("change-feed requires a service config path")?;
            supervisor::run_change_feed(&config_path)
        }
        Some("probe-path") => {
            let path = args
                .next()
                .map(PathBuf::from)
                .context("probe-path requires an absolute path argument")?;
            supervisor::probe_path_once(&path)
        }
        Some("service") => {
            let service_name = args.next().unwrap_or_else(|| SERVICE_NAME.to_string());
            let config_path = args.next().map(PathBuf::from).unwrap_or_else(default_config_path);
            start_service_dispatcher(service_name, config_path)
        }
        None => start_service_dispatcher(SERVICE_NAME.to_string(), default_config_path()),
        Some(other) => anyhow::bail!(
            "unknown command '{other}'. Use: service [service-name] [config] | console [config] | check [config] | check-remote [config] | cleanup [config] | change-feed <config> | probe-path <path>"
        ),
    }
}

fn start_service_dispatcher(service_name: String, config_path: PathBuf) -> Result<()> {
    SERVICE_RUNTIME
        .set(ServiceRuntime {
            name: service_name.clone(),
            config_path,
        })
        .map_err(|_| anyhow::anyhow!("service runtime was already initialized"))?;
    service_dispatcher::start(&service_name, ffi_service_main)
        .context("failed to start Windows service dispatcher")
}

fn default_config_path() -> PathBuf {
    env::var_os("CLOUDFOLDER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG))
}

fn run_console(config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    cfg.validate()?;
    let logger = Logger::new(
        &cfg.logging.service_log,
        cfg.logging.max_bytes,
        cfg.logging.keep_files,
    )?;
    logger.info("starting in console mode");
    let stop = Arc::new(AtomicBool::new(false));
    supervisor::run(cfg, Some(config_path), logger, stop)
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        let fallback = Logger::fallback();
        fallback.error(&format!("fatal service error: {err:#}"));
        // Return a non-zero process exit code so Windows SCM recovery actions
        // apply to unexpected service failures. Process teardown also closes
        // the KILL_ON_JOB_CLOSE job handle and therefore cannot orphan rclone.
        std::process::exit(1);
    }
}

fn run_service() -> Result<()> {
    let runtime = SERVICE_RUNTIME
        .get()
        .context("service runtime is not initialized")?;
    let config_path = runtime.config_path.clone();
    let cfg =
        Config::load(&config_path).with_context(|| format!("loading {}", config_path.display()))?;
    cfg.validate()?;
    let logger = Logger::new(
        &cfg.logging.service_log,
        cfg.logging.max_bytes,
        cfg.logging.keep_files,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_for_handler.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(&runtime.name, event_handler)
        .context("registering service control handler")?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: Duration::from_secs(30),
        process_id: None,
    })?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    logger.info(&format!(
        "service started; name={} config={}",
        runtime.name,
        config_path.display()
    ));
    let result = supervisor::run(cfg, Some(&config_path), logger.clone(), stop);

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StopPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: Duration::from_secs(20),
        process_id: None,
    })?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: if result.is_ok() {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(1)
        },
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    result
}
