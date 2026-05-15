use std::{fs, path::Path, time::UNIX_EPOCH};

use anyhow::{Context, Result};
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::{
    app::AppContext,
    hash::blake3_file,
    image_io::{ensure_supported, is_supported_image_path},
    models::NewPhoto,
    traits::BatchProcessor,
};

pub struct DiscoveryProcessor;

#[derive(Default)]
struct ScanStats {
    scanned: usize,
    new_or_modified: usize,
    unchanged: usize,
    failed: usize,
}

impl BatchProcessor for DiscoveryProcessor {
    fn name(&self) -> &'static str {
        "discovery"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let scan_id = ctx.db.begin_scan()?;
        let mut stats = ScanStats::default();

        for root in &ctx.config.library_paths {
            scan_root(ctx, scan_id, root, &mut stats)?;
        }

        let missing = ctx.db.mark_missing_not_seen(scan_id)?;
        ctx.db.finish_scan(scan_id)?;
        let (total, missing_total) = ctx.db.stats()?;

        info!(
            processor = self.name(),
            scanned = stats.scanned,
            new_or_modified = stats.new_or_modified,
            unchanged = stats.unchanged,
            failed = stats.failed,
            newly_marked_missing = missing,
            total_photos = total,
            total_missing = missing_total,
            "scan completed"
        );
        Ok(())
    }
}

fn scan_root(ctx: &AppContext, scan_id: i64, root: &Path, stats: &mut ScanStats) -> Result<()> {
    if !root.exists() {
        warn!(path = %root.display(), "library path does not exist");
        return Ok(());
    }

    for entry in WalkDir::new(root).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                stats.failed += 1;
                warn!(%error, "failed to read directory entry");
                continue;
            }
        };

        if !entry.file_type().is_file() || !is_supported_image_path(entry.path()) {
            continue;
        }

        if let Some(limit) = ctx.limit
            && stats.scanned >= limit
        {
            break;
        }

        stats.scanned += 1;
        if let Err(error) = process_file(ctx, scan_id, entry.path(), stats) {
            stats.failed += 1;
            warn!(path = %entry.path().display(), %error, "failed to scan image");
        }
    }

    Ok(())
}

fn process_file(ctx: &AppContext, scan_id: i64, path: &Path, stats: &mut ScanStats) -> Result<()> {
    ensure_supported(path)?;
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let file_size = i64::try_from(metadata.len()).context("file size exceeds i64")?;
    let modified_at_unix = metadata
        .modified()
        .context("file has no modified timestamp")?
        .duration_since(UNIX_EPOCH)
        .context("file modified timestamp is before Unix epoch")?
        .as_secs();
    let modified_at_unix = i64::try_from(modified_at_unix).context("mtime exceeds i64")?;
    let path_string = path.to_string_lossy().to_string();

    if let Some((photo_id, old_size, old_mtime)) = ctx.db.photo_file_state(&path_string)?
        && old_size == file_size
        && old_mtime == modified_at_unix
    {
        ctx.db.mark_seen_unchanged(photo_id, scan_id)?;
        stats.unchanged += 1;
        return Ok(());
    }

    let blake3_hash = blake3_file(path)?;
    let photo = NewPhoto {
        path: path_string,
        blake3_hash,
        file_size,
        modified_at_unix,
    };
    ctx.db.upsert_photo(&photo, scan_id)?;
    stats.new_or_modified += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::image_io::is_supported_image_path;

    #[test]
    fn filters_scan_paths() {
        assert!(is_supported_image_path(Path::new("photo.webp")));
        assert!(!is_supported_image_path(Path::new("video.mp4")));
    }
}
