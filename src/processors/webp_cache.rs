use std::time::Instant;

use anyhow::{Context, Result};
use libvips::VipsApp;
use rayon::prelude::*;
use tracing::{info, warn};

use crate::{app::AppContext, processors::progress, traits::BatchProcessor, webp_cache as cache};

pub struct WebpCacheProcessor;

impl BatchProcessor for WebpCacheProcessor {
    fn name(&self) -> &'static str {
        "webp_cache"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.heic_photos(ctx.limit)?;
        let pb = progress::bar(photos.len(), "webp cache");
        let cache_dir = cache::cache_dir(&ctx.config.database_path);
        let vips = VipsApp::default("iris-webp-cache").context("failed to initialize libvips")?;

        let outcomes = photos
            .par_iter()
            .map(|(photo_id, path)| {
                let start = Instant::now();
                let outcome = match cache::ensure_heic_webp(&vips, &cache_dir, path) {
                    Ok(result) => {
                        info!(
                            photo_id,
                            path = %path,
                            cache_path = %result.path.display(),
                            cache_hit = result.cache_hit,
                            elapsed_ms = start.elapsed().as_millis(),
                            "HEIC WebP cache ready"
                        );
                        if result.cache_hit {
                            WebpCacheOutcome::Cached
                        } else {
                            WebpCacheOutcome::Converted
                        }
                    }
                    Err(error) => {
                        warn!(photo_id, path = %path, %error, "failed to cache HEIC as WebP");
                        WebpCacheOutcome::Failed
                    }
                };
                pb.inc(1);
                outcome
            })
            .collect::<Vec<_>>();

        let cached = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, WebpCacheOutcome::Cached))
            .count();
        let converted = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, WebpCacheOutcome::Converted))
            .count();
        let failed = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, WebpCacheOutcome::Failed))
            .count();

        pb.finish_and_clear();
        info!(
            processor = self.name(),
            cached, converted, failed, "WebP cache completed"
        );
        Ok(())
    }
}

enum WebpCacheOutcome {
    Cached,
    Converted,
    Failed,
}
