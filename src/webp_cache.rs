use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use libvips::{VipsApp, VipsImage, ops};

const HEIC_WEBP_QUALITY: i32 = 82;

#[derive(Debug, Clone)]
pub struct CachedWebp {
    pub path: PathBuf,
    pub cache_hit: bool,
}

pub fn cache_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".iris-cache")
        .join("webp")
}

pub fn ensure_heic_webp(
    _vips: &VipsApp,
    cache_dir: &Path,
    source_path: &str,
) -> Result<CachedWebp> {
    let metadata = std::fs::metadata(source_path)
        .with_context(|| format!("failed to stat HEIC photo {source_path}"))?;
    let cache_path = cache_path(&metadata, cache_dir, source_path)?;

    if cache_path.exists() {
        return Ok(CachedWebp {
            path: cache_path,
            cache_hit: true,
        });
    }

    convert_heic_to_webp(source_path, &cache_path)?;
    Ok(CachedWebp {
        path: cache_path,
        cache_hit: false,
    })
}

pub fn cache_path(
    metadata: &std::fs::Metadata,
    cache_dir: &Path,
    source_path: &str,
) -> Result<PathBuf> {
    let modified = metadata
        .modified()
        .context("failed to read source image modified time")?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(cache_path_from_parts(
        cache_dir,
        source_path,
        metadata.len(),
        modified,
    ))
}

pub fn cache_path_from_parts(
    cache_dir: &Path,
    source_path: &str,
    file_size: u64,
    modified_at_unix: u64,
) -> PathBuf {
    let key = format!("{source_path}|{file_size}|{modified_at_unix}");
    let hash = blake3::hash(key.as_bytes()).to_hex().to_string();
    cache_dir.join(format!("{hash}.webp"))
}

pub fn is_heic_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("heic"))
}

fn convert_heic_to_webp(source_path: &str, cache_path: &Path) -> Result<()> {
    let parent = cache_path
        .parent()
        .context("HEIC WebP cache path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create WebP cache dir {}", parent.display()))?;

    let temp_path = cache_path.with_extension(format!("webp.{}.tmp", temp_suffix()));
    let result = convert_heic_to_webp_inner(source_path, &temp_path)
        .and_then(|()| move_cache_file(&temp_path, cache_path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn convert_heic_to_webp_inner(source_path: &str, temp_path: &Path) -> Result<()> {
    let temp_path_string = temp_path.to_string_lossy().into_owned();
    let image = VipsImage::new_from_file(source_path)
        .map_err(|error| anyhow::anyhow!("{error:?}"))
        .with_context(|| format!("libvips failed to load HEIC {source_path}"))?;
    let image = ops::autorot(&image)
        .map_err(|error| anyhow::anyhow!("{error:?}"))
        .context("libvips failed to autorotate HEIC")?;
    let options = ops::WebpsaveOptions {
        q: HEIC_WEBP_QUALITY,
        smart_subsample: true,
        ..Default::default()
    };
    ops::webpsave_with_opts(&image, &temp_path_string, &options)
        .map_err(|error| anyhow::anyhow!("{error:?}"))
        .with_context(|| format!("libvips failed to write WebP {}", temp_path.display()))
}

fn move_cache_file(temp_path: &Path, cache_path: &Path) -> Result<()> {
    std::fs::rename(temp_path, cache_path).with_context(|| {
        format!(
            "failed to move WebP cache {} to {}",
            temp_path.display(),
            cache_path.display()
        )
    })
}

fn temp_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}.{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_heic_paths_case_insensitively() {
        assert!(is_heic_path(Path::new("/tmp/a.HEIC")));
        assert!(!is_heic_path(Path::new("/tmp/a.jpg")));
    }

    #[test]
    fn cache_path_uses_source_identity_and_metadata() {
        let dir = Path::new("/tmp/cache");
        let first = cache_path_from_parts(dir, "/photos/a.heic", 12, 34);
        let same = cache_path_from_parts(dir, "/photos/a.heic", 12, 34);
        let changed = cache_path_from_parts(dir, "/photos/a.heic", 13, 34);

        assert_eq!(first, same);
        assert_ne!(first, changed);
        assert_eq!(first.parent(), Some(dir));
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("webp"));
    }
}
