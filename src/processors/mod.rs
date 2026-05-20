mod discovery;
mod embeddings;
mod faces;
mod geo;
mod image_quality;
mod metadata;
mod ocr;
mod progress;
mod webp_cache;

use anyhow::Result;

use crate::{app::AppContext, traits::BatchProcessor};

pub use discovery::DiscoveryProcessor;
pub use embeddings::{ImageEmbeddingProcessor, OcrTextEmbeddingProcessor};
pub use faces::FaceEmbeddingProcessor;
pub use geo::ReverseGeoProcessor;
pub use image_quality::QualityProcessor;
pub use metadata::MetadataProcessor;
pub use ocr::LazyOcrProcessor;
pub use webp_cache::WebpCacheProcessor;

pub fn run_index(ctx: &AppContext) -> Result<()> {
    DiscoveryProcessor.run(ctx)?;
    MetadataProcessor.run(ctx)?;
    ReverseGeoProcessor.run(ctx)?;
    QualityProcessor.run(ctx)?;
    WebpCacheProcessor.run(ctx)?;
    ImageEmbeddingProcessor.run(ctx)?;
    FaceEmbeddingProcessor.run(ctx)?;
    LazyOcrProcessor.run(ctx)?;
    OcrTextEmbeddingProcessor.run(ctx)?;
    Ok(())
}
