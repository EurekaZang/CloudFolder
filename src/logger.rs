use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    max_bytes: u64,
    keep_files: usize,
    gate: Mutex<()>,
}

impl Logger {
    pub fn new(path: &Path, max_bytes: u64, keep_files: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating log directory {}", parent.display()))?;
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                max_bytes: max_bytes.max(1024 * 1024),
                keep_files: keep_files.max(1),
                gate: Mutex::new(()),
            }),
        })
    }

    pub fn fallback() -> Self {
        Self::new(
            Path::new(r"C:\ProgramData\CloudFolder\fatal.log"),
            10 * 1024 * 1024,
            2,
        )
        .unwrap_or_else(|_| Self {
            inner: Arc::new(Inner {
                path: PathBuf::from("CloudFolderService-fatal.log"),
                max_bytes: 10 * 1024 * 1024,
                keep_files: 2,
                gate: Mutex::new(()),
            }),
        })
    }

    pub fn info(&self, message: &str) {
        self.write("INFO", message);
    }
    pub fn warn(&self, message: &str) {
        self.write("WARN", message);
    }
    pub fn error(&self, message: &str) {
        self.write("ERROR", message);
    }

    fn write(&self, level: &str, message: &str) {
        let _guard = self.inner.gate.lock().ok();
        if self.should_rotate() {
            let _ = self.rotate();
        }
        let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
        let line = format!("{timestamp} [{level}] {message}\r\n");
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.path)
        {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    fn should_rotate(&self) -> bool {
        fs::metadata(&self.inner.path)
            .map(|m| m.len() >= self.inner.max_bytes)
            .unwrap_or(false)
    }

    fn rotate(&self) -> Result<()> {
        for index in (1..=self.inner.keep_files).rev() {
            let src = rotated_path(&self.inner.path, index);
            if index == self.inner.keep_files {
                let _ = fs::remove_file(&src);
            } else if src.exists() {
                let dst = rotated_path(&self.inner.path, index + 1);
                let _ = fs::rename(&src, &dst);
            }
        }
        if self.inner.path.exists() {
            fs::rename(&self.inner.path, rotated_path(&self.inner.path, 1))?;
        }
        Ok(())
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}
