use anyhow::{Context, Result};
use open_clip_inference::{TextEmbedder, VisionEmbedder};
use rayon::prelude::*;
use tracing::{info, warn};

use crate::{app::AppContext, image_io::open_image, processors::progress, traits::BatchProcessor};

const MOBILE_CLIP_MODEL_ID: &str = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";
const CLIP_IMAGE_BATCH_SIZE: usize = 16;
const CLIP_TEXT_BATCH_SIZE: usize = 64;

pub struct ImageEmbeddingProcessor;
pub struct OcrTextEmbeddingProcessor;

impl BatchProcessor for ImageEmbeddingProcessor {
    fn name(&self) -> &'static str {
        "image_embeddings"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.photos_missing_image_embedding(ctx.limit)?;
        let pb = progress::bar(photos.len(), "image embeddings");
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let embedder = runtime
            .block_on(VisionEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to initialize MobileCLIP vision embedder")?;

        let mut done = 0;
        let mut failed = 0;

        for chunk in photos.chunks(CLIP_IMAGE_BATCH_SIZE) {
            let decoded = chunk
                .par_iter()
                .map(|photo| {
                    (
                        photo.id,
                        photo.path.clone(),
                        open_image(photo.path.as_ref()),
                    )
                })
                .collect::<Vec<_>>();

            let mut batch = Vec::new();
            for (photo_id, path, result) in decoded {
                match result {
                    Ok(img) => batch.push((photo_id, path, img)),
                    Err(error) => {
                        failed += 1;
                        pb.inc(1);
                        warn!(path = %path, %error, "image embedding failed");
                    }
                }
            }

            if batch.is_empty() {
                continue;
            }

            let images = batch
                .iter()
                .map(|(_, _, img)| img.clone())
                .collect::<Vec<_>>();

            match embedder
                .embed_images(&images)
                .context("failed to generate image embeddings")
            {
                Ok(embeddings) => {
                    for ((photo_id, _path, _img), embedding) in
                        batch.iter().zip(embeddings.outer_iter())
                    {
                        let vector = embedding.iter().copied().collect::<Vec<_>>();
                        ctx.db.save_image_embedding(*photo_id, &vector)?;
                        done += 1;
                        pb.inc(1);
                    }
                }
                Err(error) => {
                    failed += batch.len();
                    for (_, path, _) in batch {
                        pb.inc(1);
                        warn!(path = %path, %error, "image embedding failed");
                    }
                }
            }
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
        let photos = ctx.db.photos_missing_ocr_text_embedding(ctx.limit)?;
        let pb = progress::bar(photos.len(), "text embeddings");
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let embedder = runtime
            .block_on(TextEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to initialize MobileCLIP text embedder")?;

        let mut done = 0;
        let mut failed = 0;

        for chunk in photos.chunks(CLIP_TEXT_BATCH_SIZE) {
            let texts = chunk
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>();

            match embedder
                .embed_texts(&texts)
                .context("failed to generate OCR text embeddings")
            {
                Ok(embeddings) => {
                    for ((photo, _text), embedding) in chunk.iter().zip(embeddings.outer_iter()) {
                        let vector = embedding.iter().copied().collect::<Vec<_>>();
                        ctx.db.save_ocr_text_embedding(photo.id, &vector)?;
                        done += 1;
                        pb.inc(1);
                    }
                }
                Err(error) => {
                    failed += chunk.len();
                    for (photo, _text) in chunk {
                        pb.inc(1);
                        warn!(path = %photo.path, %error, "OCR text embedding failed");
                    }
                }
            }
        }

        pb.finish_and_clear();

        info!(
            processor = self.name(),
            done, failed, "OCR text embeddings completed"
        );
        Ok(())
    }
}
