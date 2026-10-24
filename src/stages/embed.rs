use anyhow::{Context, Result};
use open_clip_inference::{TextEmbedder, VisionEmbedder};
use rayon::prelude::*;

use crate::jobs::{CancelToken, Cancelled, ProgressSink, Stage, StageCtx};

const MOBILE_CLIP_MODEL_ID: &str = "RuteNL/MobileCLIP2-S3-OpenCLIP-ONNX";
const BATCH: usize = 16;
/// CLIP's logit scale, to turn cosine similarities into zero-shot probabilities.
const LOGIT_SCALE: f32 = 100.0;

const AESTHETIC_PROMPTS: [&str; 4] = [
    "a beautiful professional wedding photograph",
    "a blurry, badly lit amateur photo",
    "a screenshot of a phone screen",
    "a photo of a document or a text message",
];

/// CLIP image embeddings (search, Shots, Chapters, diversity) and zero-shot scores.
pub struct Embed;

impl Stage for Embed {
    fn name(&self) -> &'static str {
        "embed"
    }

    fn title(&self) -> &'static str {
        "Image embeddings"
    }

    fn description(&self) -> &'static str {
        "MobileCLIP embeddings for search, Shots and Chapters, plus zero-shot aesthetic and screenshot scores."
    }

    fn version(&self) -> i64 {
        1
    }

    fn deps(&self) -> &'static [&'static str] {
        &["ingest"]
    }

    fn per_photo(&self) -> bool {
        true
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        let photos = ctx.db.photos_pending_stage(self.name(), self.version(), true)?;
        sink.start(photos.len());
        if photos.is_empty() {
            return Ok(());
        }
        sink.message("loading MobileCLIP");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let vision = runtime
            .block_on(VisionEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to load the MobileCLIP vision model")?;
        let prompts = ClipText::load()?.embed(&AESTHETIC_PROMPTS)?;
        sink.message("embedding");

        for chunk in photos.chunks(BATCH) {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let decoded: Vec<_> = chunk.par_iter().map(|photo| (photo, super::load_preview(ctx, photo))).collect();
            let mut batch = Vec::new();
            for (photo, image) in decoded {
                match image {
                    Ok(image) => batch.push((photo, image)),
                    Err(error) => {
                        let reason = format!("{error:#}");
                        sink.item_failed(photo.id, &reason);
                        ctx.db.mark_photo_stage(photo.id, self.name(), self.version(), Some(&reason))?;
                        sink.inc(1);
                    }
                }
            }
            if batch.is_empty() {
                continue;
            }
            let images: Vec<_> = batch.iter().map(|(_, image)| image.clone()).collect();
            let embeddings = vision.embed_images(&images).context("CLIP failed")?;
            for ((photo, _), row) in batch.iter().zip(embeddings.outer_iter()) {
                let vector = normalize(row.iter().copied().collect());
                let probs = softmax(&prompts.iter().map(|p| dot(p, &vector) * LOGIT_SCALE).collect::<Vec<_>>());
                ctx.db.save_image_embedding(photo.id, &vector)?;
                ctx.db.save_clip_scores(photo.id, f64::from(probs[0]), f64::from(probs[2] + probs[3]))?;
                ctx.db.mark_photo_stage(photo.id, self.name(), self.version(), None)?;
                sink.inc(1);
            }
        }
        Ok(())
    }
}

/// The CLIP text side: prompts and search queries in the same space as the images.
pub struct ClipText(TextEmbedder);

impl ClipText {
    pub fn load() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let embedder = runtime
            .block_on(TextEmbedder::from_hf(MOBILE_CLIP_MODEL_ID).build())
            .context("failed to load the MobileCLIP text model")?;
        Ok(Self(embedder))
    }

    /// Unit-length embeddings of the given texts.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let embeddings = self.0.embed_texts(texts).context("CLIP text embedding failed")?;
        Ok(embeddings.outer_iter().map(|row| normalize(row.iter().copied().collect())).collect())
    }
}

pub fn normalize(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        vector.iter_mut().for_each(|v| *v /= norm);
    }
    vector
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.into_iter().map(|e| e / sum).collect()
}
