use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_path: PathBuf,
    pub library_paths: Vec<PathBuf>,
    pub ocr_models_dir: PathBuf,
    pub ocr_edge_density_threshold: f64,
    pub scan_batch_size: usize,
    pub process_batch_size: usize,
    pub embedding_dimensions: usize,
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let mut config: Self = toml::from_str(&raw).context("failed to parse TOML config")?;
        let cwd = std::env::current_dir().context("failed to read current directory")?;
        config.normalize(&cwd);
        config.validate()?;
        Ok(config)
    }

    fn normalize(&mut self, base: &Path) {
        self.database_path = absolutize(base, &self.database_path);
        self.ocr_models_dir = absolutize(base, &self.ocr_models_dir);
        self.library_paths = self
            .library_paths
            .iter()
            .map(|path| absolutize(base, path))
            .collect();
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.library_paths.is_empty(),
            "library_paths must not be empty"
        );
        anyhow::ensure!(self.scan_batch_size > 0, "scan_batch_size must be > 0");
        anyhow::ensure!(
            self.process_batch_size > 0,
            "process_batch_size must be > 0"
        );
        anyhow::ensure!(
            self.embedding_dimensions > 0,
            "embedding_dimensions must be > 0"
        );
        Ok(())
    }
}

fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config() {
        let config: Config = toml::from_str(
            r#"
database_path = "iris.db"
library_paths = ["./Photos"]
ocr_models_dir = "./models"
ocr_edge_density_threshold = 0.08
scan_batch_size = 500
process_batch_size = 128
embedding_dimensions = 512
"#,
        )
        .unwrap();

        assert_eq!(config.library_paths.len(), 1);
        assert_eq!(config.embedding_dimensions, 512);
    }
}
