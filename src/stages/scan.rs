use std::{fs, path::Path, time::UNIX_EPOCH};

use anyhow::{Context, Result};
use iris_core::{capture::sent_at_from_file_name, model::Source};
use walkdir::WalkDir;

use crate::{
    db::ScannedPhoto,
    image_io::is_supported_image_path,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
};

/// Finds the Photos in both folders. Videos are skipped.
pub struct Scan;

impl Stage for Scan {
    fn name(&self) -> &'static str {
        "scan"
    }

    fn title(&self) -> &'static str {
        "Scan folders"
    }

    fn description(&self) -> &'static str {
        "Lists the photographer's and guests' photos. Videos are skipped."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &[]
    }

    fn per_photo(&self) -> bool {
        false
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let roots = [
            (Source::Photographer, &ctx.config.photographer_root),
            (Source::Guest, &ctx.config.guests_root),
        ];
        let mut files = Vec::new();
        let mut videos = 0;
        sink.message("listing folders");
        for (source, root) in roots {
            anyhow::ensure!(root.exists(), "folder not found: {} (is the drive mounted?)", root.display());
            for entry in WalkDir::new(root).follow_links(false) {
                let entry = entry.with_context(|| format!("failed to list {}", root.display()))?;
                if !entry.file_type().is_file() || entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                if is_supported_image_path(entry.path()) {
                    files.push((source, root.clone(), entry.into_path()));
                } else if is_video(entry.path()) {
                    videos += 1;
                }
            }
        }

        let scan_id = ctx.db.begin_scan()?;
        sink.start(files.len());
        let mut changed = 0;
        for (source, root, path) in &files {
            cancel.check()?;
            if record(ctx, scan_id, *source, root, path)? {
                changed += 1;
            }
            sink.inc(1);
        }
        let missing = ctx.db.mark_missing_not_seen(scan_id)?;
        ctx.db.finish_scan(scan_id)?;
        ctx.db.put_blob("scan_summary", &serde_json::json!({ "photos": files.len(), "videos_skipped": videos }))?;
        sink.message(&format!(
            "{} photos ({changed} new or changed, {missing} gone), {videos} videos skipped",
            files.len()
        ));
        Ok(())
    }
}

/// Returns whether the Photo is new or changed.
fn record(ctx: &StageCtx, scan_id: i64, source: Source, root: &Path, path: &Path) -> Result<bool> {
    let metadata = fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let file_size = i64::try_from(metadata.len())?;
    let modified_at_unix = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64);
    let path_string = path.to_string_lossy().to_string();

    if let Some((id, size, mtime)) = ctx.db.photo_file_state(&path_string)?
        && size == file_size
        && mtime == modified_at_unix
    {
        ctx.db.mark_seen_unchanged(id, scan_id)?;
        return Ok(false);
    }

    let file_name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
    let source_folder = match source {
        Source::Photographer => path
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.components().next())
            .filter(|_| path.parent() != Some(root))
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default(),
        Source::Guest => String::new(),
    };
    let photo = ScannedPhoto {
        sent_at_utc: (source == Source::Guest).then(|| sent_at_from_file_name(&file_name)).flatten(),
        sequence: (source == Source::Photographer).then(|| camera_sequence(&file_name)).flatten(),
        path: path_string,
        file_name,
        source,
        source_folder,
        file_size,
        modified_at_unix,
    };
    ctx.db.upsert_scanned_photo(&photo, scan_id)?;
    Ok(true)
}

/// The camera's file number: `DSC_9562.jpg` → 9562.
fn camera_sequence(file_name: &str) -> Option<i64> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
    let digits: String = stem.chars().rev().take_while(char::is_ascii_digit).collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "mov" | "m4v" | "avi"))
}
