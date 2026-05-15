use anyhow::{Context, Result};
use ocr_rs::{OcrEngine, OcrEngineConfig};
use tracing::{info, warn};

use crate::{
    app::AppContext,
    image_io::open_image,
    models::OcrResult,
    processors::{image_quality::screenshot_score, progress},
    text::cleanup_text,
    traits::BatchProcessor,
};

pub struct LazyOcrProcessor;

impl BatchProcessor for LazyOcrProcessor {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.photos_pending_ocr(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "ocr");
        let engine = OcrEngine::new(
            ctx.config.ocr_models_dir.join("PP-OCRv5_mobile_det.mnn"),
            ctx.config
                .ocr_models_dir
                .join("latin_PP-OCRv5_mobile_rec_infer.mnn"),
            ctx.config.ocr_models_dir.join("ppocr_keys_latin.txt"),
            Some(OcrEngineConfig::fast()),
        )
        .context("failed to initialize OCR engine")?;

        let mut done = 0;
        let mut skipped = 0;
        let mut failed = 0;

        for photo in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            let img = match open_image(photo.path.as_ref()) {
                Ok(img) => img,
                Err(error) => {
                    failed += 1;
                    ctx.db.mark_ocr_failed(photo.id)?;
                    warn!(path = %photo.path, %error, "failed to decode image for OCR");
                    pb.inc(1);
                    continue;
                }
            };

            if screenshot_score(&img) < ctx.config.ocr_edge_density_threshold {
                skipped += 1;
                ctx.db.mark_ocr_skipped(photo.id)?;
                pb.inc(1);
                continue;
            }

            match engine.recognize(&img) {
                Ok(results) => {
                    let raw_text = results
                        .iter()
                        .map(|result| result.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let confidence =
                        average_confidence(results.iter().map(|result| result.confidence));
                    let cleaned_text = cleanup_text(&raw_text);
                    ctx.db.save_ocr_result(
                        photo.id,
                        &OcrResult {
                            raw_text,
                            cleaned_text,
                            confidence,
                        },
                    )?;
                    done += 1;
                }
                Err(error) => {
                    failed += 1;
                    ctx.db.mark_ocr_failed(photo.id)?;
                    warn!(path = %photo.path, %error, "OCR failed");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();

        info!(
            processor = self.name(),
            done, skipped, failed, "OCR completed"
        );
        Ok(())
    }
}

fn average_confidence(values: impl Iterator<Item = f32>) -> Option<f64> {
    let mut count = 0_u64;
    let mut sum = 0.0_f64;
    for value in values {
        count += 1;
        sum += f64::from(value);
    }
    (count > 0).then_some(sum / count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_confidence() {
        assert_eq!(average_confidence([0.5, 1.0].into_iter()), Some(0.75));
        assert_eq!(average_confidence([].into_iter()), None);
    }
}
