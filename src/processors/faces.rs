use anyhow::{Context, Result};
use face_id::analyzer::{FaceAnalysis, FaceAnalyzer};
use tracing::{info, warn};

use crate::{
    app::AppContext, image_io::open_image, models::FaceDetection, processors::progress,
    traits::BatchProcessor,
};

pub struct FaceEmbeddingProcessor;

impl BatchProcessor for FaceEmbeddingProcessor {
    fn name(&self) -> &'static str {
        "faces"
    }

    fn run(&self, ctx: &AppContext) -> Result<()> {
        let photos = ctx.db.photos_pending_faces(ctx.effective_limit())?;
        let pb = progress::bar(photos.len(), "faces");
        let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
        let analyzer = runtime
            .block_on(FaceAnalyzer::from_hf().build())
            .context("failed to initialize face analyzer")?;

        let mut photos_done = 0;
        let mut faces_done = 0;
        let mut failed = 0;

        for photo in photos {
            let _ = (&photo.blake3_hash, photo.width, photo.height);
            let result = (|| -> Result<Vec<FaceDetection>> {
                let img = open_image(photo.path.as_ref())?;
                let faces = analyzer
                    .analyze(&img)
                    .context("failed to detect and embed faces")?;
                face_detections(faces)
            })();

            match result {
                Ok(faces) => {
                    faces_done += faces.len();
                    ctx.db.replace_faces(photo.id, &faces)?;
                    photos_done += 1;
                }
                Err(error) => {
                    failed += 1;
                    ctx.db.mark_faces_failed(photo.id)?;
                    warn!(path = %photo.path, %error, "face processing failed");
                }
            }

            pb.inc(1);
        }

        pb.finish_and_clear();
        info!(
            processor = self.name(),
            photos_done, faces_done, failed, "face processing completed"
        );
        Ok(())
    }
}

fn face_detections(faces: Vec<FaceAnalysis>) -> Result<Vec<FaceDetection>> {
    faces
        .into_iter()
        .enumerate()
        .map(|(index, face)| {
            let face_index = i64::try_from(index).context("face index exceeds i64")?;
            Ok(FaceDetection {
                face_index,
                bbox_x1: f64::from(face.detection.bbox.x1),
                bbox_y1: f64::from(face.detection.bbox.y1),
                bbox_x2: f64::from(face.detection.bbox.x2),
                bbox_y2: f64::from(face.detection.bbox.y2),
                detection_score: f64::from(face.detection.score),
                landmarks_json: face
                    .detection
                    .landmarks
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("failed to serialize face landmarks")?,
                gender: Some(format!("{:?}", face.gender)),
                age: Some(face.age),
                embedding: face.embedding,
            })
        })
        .collect()
}
