use anyhow::{Context, Result};
use open_clip_inference::{TextEmbedder, VisionEmbedder};
use tracing::{info, warn};

use crate::{app::AppContext, image_io::open_image, processors::progress, traits::BatchProcessor};

const MOBILE_CLIP_MODEL_ID: &str = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";

pub struct ImageEmbeddingProcessor;
pub struct OcrTextEmbeddingProcessor;

impl BatchProcessor for ImageEmbeddingProcessor {
    fn name(&self) -> &'static str {
        "image_embeddings"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx
            .db
            .photos_missing_image_embedding(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "image embeddings");
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let embedder = runtime
            .block_on(VisionEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to initialize MobileCLIP vision embedder")?;

        let mut done = 0;
        let mut failed = 0;

        for photo in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            let result = (|| -> Result<()> {
                let img = open_image(photo.path.as_ref())?;
                let embedding = embedder
                    .embed_image(&img)
                    .context("failed to generate image embedding")?;
                let vector = embedding.iter().copied().collect::<Vec<_>>();
                ctx.db.save_image_embedding(photo.id, &vector)?;
                Ok(())
            })();

            match result {
                Ok(()) => {
                    done += 1;
                }
                Err(error) => {
                    failed += 1;
                    warn!(path = %photo.path, %error, "image embedding failed");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();

        info!(
            processor = self.name(),
            done, failed, "image embeddings completed"
        );
        Ok(())
    }
}

impl BatchProcessor for OcrTextEmbeddingProcessor {
    fn name(&self) -> &'static str {
        "ocr_text_embeddings"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx
            .db
            .photos_missing_ocr_text_embedding(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "text embeddings");
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let embedder = runtime
            .block_on(TextEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to initialize MobileCLIP text embedder")?;

        let mut done = 0;
        let mut failed = 0;

        for (photo, text) in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            let result = (|| -> Result<()> {
                let embedding = embedder
                    .embed_text(&text)
                    .context("failed to generate OCR text embedding")?;
                let vector = embedding.iter().copied().collect::<Vec<_>>();
                ctx.db.save_ocr_text_embedding(photo.id, &vector)?;
                Ok(())
            })();

            match result {
                Ok(()) => {
                    done += 1;
                }
                Err(error) => {
                    failed += 1;
                    warn!(path = %photo.path, %error, "OCR text embedding failed");
                }
            }
            pb.inc(1);
        }

        pb.finish_and_clear();

        info!(
            processor = self.name(),
            done, failed, "OCR text embeddings completed"
        );
        Ok(())
    }
}
