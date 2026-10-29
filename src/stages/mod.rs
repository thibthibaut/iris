//! Every pipeline step is a `Stage`, run as a job from the GUI or from the CLI.

mod chapters;
mod cluster;
pub mod embed;
mod export;
mod faces;
mod ingest;
mod quality;
mod scan;
mod select;
mod shots;
pub mod vlm;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use anyhow::Result;
use rayon::prelude::*;

use crate::{
    db::PhotoRecord,
    jobs::{CancelToken, Cancelled, ProgressSink, Stage, StageCtx},
};

pub use embed::ClipText;

/// All stages, in dependency order.
pub fn all() -> Vec<Arc<dyn Stage>> {
    let stages: Vec<Arc<dyn Stage>> = vec![
        Arc::new(scan::Scan),
        Arc::new(ingest::Ingest),
        Arc::new(quality::Quality),
        Arc::new(embed::Embed),
        Arc::new(faces::Faces),
        Arc::new(cluster::Cluster),
        Arc::new(shots::Shots),
        Arc::new(chapters::Chapters),
        Arc::new(vlm::Vlm(vlm::Task::ChapterTiebreak)),
        Arc::new(select::Select),
        Arc::new(vlm::Vlm(vlm::Task::PickReview)),
    ];
    stages
        .into_iter()
    .chain(export::Export::all(&["ours", "bride_parents", "dad", "mom"]).into_iter().map(|e| Arc::new(e) as Arc<dyn Stage>))
    .collect()
}

/// Export stages for Editions added in the GUI after the worker was started.
pub fn dynamic_export(name: &str) -> Option<Arc<dyn Stage>> {
    let edition = name.strip_prefix("export:")?;
    let mut bytes = edition.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase()) || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
        return None;
    }
    Some(Arc::new(export::Export::for_edition(edition)))
}

/// Runs `work` on every pending Photo of a per-Photo stage, `threads` at a time, and
/// records each Photo's outcome. Failures are per Photo; they never stop the stage.
pub fn for_each_pending<F>(
    ctx: &StageCtx,
    stage: &dyn Stage,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
    threads: usize,
    work: F,
) -> Result<()>
where
    F: Fn(&PhotoRecord) -> Result<()> + Sync,
{
    let photos = ctx.db.photos_pending_stage(stage.name(), stage.version(), true)?;
    sink.start(photos.len());
    let failed = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads.max(1)).build()?;
    pool.install(|| {
        photos.par_iter().for_each(|photo| {
            if cancel.is_cancelled() {
                return;
            }
            let result = work(photo);
            let error = result.as_ref().err().map(|error| format!("{error:#}"));
            if let Some(error) = &error {
                failed.fetch_add(1, Ordering::Relaxed);
                sink.item_failed(photo.id, error);
            }
            if let Err(db_error) = ctx.db.mark_photo_stage(photo.id, stage.name(), stage.version(), error.as_deref()) {
                tracing::warn!(%db_error, "failed to record stage progress");
            }
            sink.inc(1);
        });
    });
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let failed = failed.load(Ordering::Relaxed);
    anyhow::ensure!(
        photos.is_empty() || failed < photos.len(),
        "every photo failed; see the failures list for the reason"
    );
    sink.message(&format!("{} photos, {failed} failed", photos.len()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamically_resolves_export_stages_for_added_editions() {
        let stage = dynamic_export("export:edition_123").unwrap();
        assert_eq!(stage.name(), "export:edition_123");
        assert!(dynamic_export("export:edition/123").is_none());
    }
}

/// The 1200 px WebP proxy of a Photo, decoded (upright, sRGB).
pub fn load_preview(ctx: &StageCtx, photo: &PhotoRecord) -> Result<image::DynamicImage> {
    let hash = photo.blake3_hash.as_deref().ok_or_else(|| anyhow::anyhow!("not ingested yet"))?;
    let path = crate::image_io::proxy_path(&ctx.config.webp_dir(), hash, crate::image_io::PREVIEW_SIZE);
    image::open(&path).map_err(|error| anyhow::anyhow!("failed to open preview {}: {error}", path.display()))
}
