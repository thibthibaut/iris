//! Bootstrap configuration: only where things live. Every other setting is in the DB,
//! edited from the GUI (see `settings`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_path: PathBuf,
    pub photographer_root: PathBuf,
    pub guests_root: PathBuf,
    /// WebP thumbnails and other derived files.
    pub cache_dir: PathBuf,
    /// Versioned print PDFs.
    pub exports_dir: PathBuf,
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let mut config: Self = toml::from_str(&raw).context("failed to parse TOML config")?;
        // Absolute paths: Typst and other tools resolve relative ones from elsewhere.
        let base = std::env::current_dir()?.join(path.parent().unwrap_or_else(|| Path::new(".")));
        for path in [
            &mut config.database_path,
            &mut config.photographer_root,
            &mut config.guests_root,
            &mut config.cache_dir,
            &mut config.exports_dir,
        ] {
            *path = normalize(&absolutize(&base, path));
        }
        Ok(config)
    }

    pub fn webp_dir(&self) -> PathBuf {
        self.cache_dir.join("webp")
    }

    pub fn preview_dir(&self) -> PathBuf {
        self.cache_dir.join("previews")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.cache_dir.join("tmp")
    }
}

/// Resolves `..` and `.` without touching the disk (the mounts may be slow or absent).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}
