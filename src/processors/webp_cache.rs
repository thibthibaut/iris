use std::{sync::Mutex, time::Instant};

use anyhow::{Context, Result};
use libvips::VipsApp;
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
        let vips = Mutex::new(
            VipsApp::default("iris-webp-cache").context("failed to initialize libvips")?,
        );
        let mut cached = 0;
        let mut converted = 0;
        let mut failed = 0;

        for (photo_id, path) in photos {
            let start = Instant::now();
            match cache::ensure_heic_webp(&vips, &cache_dir, &path) {
                Ok(result) => {
                    if result.cache_hit {
                        cached += 1;
                    } else {
                        converted += 1;
                    }
                    info!(
                        photo_id,
                        path = %path,
                        cache_path = %result.path.display(),
                        cache_hit = result.cache_hit,
                        elapsed_ms = start.elapsed().as_millis(),
                        "HEIC WebP cache ready"
                    );
                }
                Err(error) => {
                    failed += 1;
                    warn!(photo_id, path = %path, %error, "failed to cache HEIC as WebP");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();
        info!(
            processor = self.name(),
            cached, converted, failed, "WebP cache completed"
        );
        Ok(())
    }
}
