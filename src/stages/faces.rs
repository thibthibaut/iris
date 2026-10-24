use std::sync::Mutex;

use anyhow::{Context, Result};
use face_id::analyzer::FaceAnalyzer;
use image::imageops::FilterType;

use crate::{
    db::NewFace,
    jobs::{CancelToken, ProgressSink, Stage, StageCtx},
    stages::{for_each_pending, load_preview, quality::tenengrad},
};

/// Face detection and identity embeddings, on the upright 1200 px proxy.
pub struct Faces;

impl Stage for Faces {
    fn name(&self) -> &'static str {
        "faces"
    }

    fn title(&self) -> &'static str {
        "Detect faces"
    }

    fn description(&self) -> &'static str {
        "Finds faces, their identity embedding and their sharpness."
    }

    fn version(&self) -> i64 {
        2
    }

    fn deps(&self) -> &'static [&'static str] {
        &["ingest"]
    }

    fn per_photo(&self) -> bool {
        true
    }

    fn run(&self, ctx: &StageCtx, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<()> {
        if ctx.db.photos_pending_stage(self.name(), self.version(), true)?.is_empty() {
            sink.start(0);
            return Ok(());
        }
        sink.message("loading face models");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let analyzer = Mutex::new(
            runtime
                .block_on(FaceAnalyzer::from_hf().build())
                .context("failed to load the face models")?,
        );
        sink.message("detecting faces");
        for_each_pending(ctx, self, sink, cancel, 2, |photo| {
            let image = load_preview(ctx, photo)?;
            let analyses = analyzer.lock().unwrap().analyze(&image).context("face analysis failed")?;
            // face_id gives boxes as fractions of the image: scale them to the upright original
            // (stored) and to the proxy (for the sharpness crop).
            let (iw, ih) = (f64::from(image.width()), f64::from(image.height()));
            let (ow, oh) = (photo.width.map_or(iw, |w| w as f64), photo.height.map_or(ih, |h| h as f64));
            let faces = analyses
                .into_iter()
                .map(|face| {
                    let b = &face.detection.bbox;
                    let x1 = f64::from(b.x1).clamp(0.0, 1.0);
                    let y1 = f64::from(b.y1).clamp(0.0, 1.0);
                    let x2 = f64::from(b.x2).clamp(0.0, 1.0);
                    let y2 = f64::from(b.y2).clamp(0.0, 1.0);
                    let crop = image.crop_imm(
                        (x1 * iw) as u32,
                        (y1 * ih) as u32,
                        (((x2 - x1) * iw) as u32).max(1),
                        (((y2 - y1) * ih) as u32).max(1),
                    );
                    Ok(NewFace {
                        bbox: [x1 * ow, y1 * oh, x2 * ow, y2 * oh],
                        detection_score: f64::from(face.detection.score),
                        landmarks_json: face
                            .detection
                            .landmarks
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()?,
                        // Normalised so a sharp face scores about 1 whatever its size.
                        sharpness: (tenengrad(&crop.resize_exact(128, 128, FilterType::Triangle)) / 4000.0).min(1.5),
                        embedding: face.embedding,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            ctx.db.replace_faces(photo.id, &faces)
        })
    }
}
