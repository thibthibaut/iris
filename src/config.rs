use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub database_path: PathBuf,
    pub library_paths: Vec<PathBuf>,
    pub ocr_models_dir: PathBuf,
    pub ocr_edge_density_threshold: f64,
    #[serde(default)]
    pub faces: FaceConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FaceConfig {
    #[serde(default = "default_face_min_cluster_size")]
    pub min_cluster_size: usize,
}

impl Default for FaceConfig {
    fn default() -> Self {
        Self {
            min_cluster_size: default_face_min_cluster_size(),
        }
    }
}

fn default_face_min_cluster_size() -> usize {
    6
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
        anyhow::ensure!(
            self.faces.min_cluster_size >= 2,
            "faces.min_cluster_size must be >= 2"
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

[faces]
min_cluster_size = 8
"#,
        )
        .unwrap();

        assert_eq!(config.library_paths.len(), 1);
        assert_eq!(config.ocr_edge_density_threshold, 0.08);
        assert_eq!(config.faces.min_cluster_size, 8);
    }

    #[test]
    fn defaults_face_config() {
        let config: Config = toml::from_str(
            r#"
database_path = "iris.db"
library_paths = ["./Photos"]
ocr_models_dir = "./models"
ocr_edge_density_threshold = 0.08
"#,
        )
        .unwrap();

        assert_eq!(config.faces.min_cluster_size, 6);
    }

    #[test]
    fn partial_faces_config_uses_defaults() {
        let config: Config = toml::from_str(
            r#"
database_path = "iris.db"
library_paths = ["./Photos"]
ocr_models_dir = "./models"
ocr_edge_density_threshold = 0.08

[faces]
"#,
        )
        .unwrap();

        assert_eq!(config.faces.min_cluster_size, 6);
    }
}
