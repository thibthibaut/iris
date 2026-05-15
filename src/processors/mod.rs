mod discovery;
mod embeddings;
mod geo;
mod image_quality;
mod metadata;
mod ocr;

use anyhow::Result;

use crate::{app::AppContext, traits::BatchProcessor};

pub use discovery::DiscoveryProcessor;
pub use embeddings::{ImageEmbeddingProcessor, OcrTextEmbeddingProcessor};
pub use geo::ReverseGeoProcessor;
pub use image_quality::QualityProcessor;
pub use metadata::MetadataProcessor;
pub use ocr::LazyOcrProcessor;

pub fn run_all(ctx: &AppContext) -> Result<()> {
    DiscoveryProcessor.run(ctx)?;
    MetadataProcessor.run(ctx)?;
    ReverseGeoProcessor.run(ctx)?;
    QualityProcessor.run(ctx)?;
    ImageEmbeddingProcessor.run(ctx)?;
    LazyOcrProcessor.run(ctx)?;
    OcrTextEmbeddingProcessor.run(ctx)?;
    Ok(())
}
