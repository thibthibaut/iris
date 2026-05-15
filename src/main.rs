mod app;
mod cli;
mod config;
mod db;
mod hash;
mod image_io;
mod models;
mod processors;
mod text;
mod traits;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

use crate::app::AppContext;
use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::db::Database;
use crate::processors::{
    DiscoveryProcessor, FaceEmbeddingProcessor, ImageEmbeddingProcessor, LazyOcrProcessor,
    MetadataProcessor, OcrTextEmbeddingProcessor, QualityProcessor, ReverseGeoProcessor, run_all,
};
use crate::traits::BatchProcessor;

fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("iris=info".parse()?))
        .init();

    let cli = Cli::parse();
    let config = Config::from_path(&cli.config)?;
    let db = Database::open(&config.database_path, config.embedding_dimensions)?;
    let ctx = AppContext::new(config, db, cli.limit);

    match cli.command {
        Command::Scan => DiscoveryProcessor.run(&ctx),
        Command::Metadata => MetadataProcessor.run(&ctx),
        Command::Geo => ReverseGeoProcessor.run(&ctx),
        Command::Quality => QualityProcessor.run(&ctx),
        Command::Embed => {
            ImageEmbeddingProcessor.run(&ctx)?;
            OcrTextEmbeddingProcessor.run(&ctx)
        }
        Command::Faces => FaceEmbeddingProcessor.run(&ctx),
        Command::Ocr => LazyOcrProcessor.run(&ctx),
        Command::ShowDb => show_db(&ctx),
        Command::All => run_all(&ctx),
    }
}

fn show_db(ctx: &AppContext) -> Result<()> {
    let rows = ctx.db.photo_rows(ctx.effective_limit())?;

    println!(
        "{:<5} {:<7} {:<10} {:<10} {:<8} {:<8} {:<7} {:<7} {:<5} {:<28} path",
        "id", "missing", "size", "mtime", "quality", "ocr", "imgvec", "txtvec", "faces", "geo"
    );
    println!("{}", "-".repeat(128));

    for row in rows {
        println!(
            "{:<5} {:<7} {:<10} {:<10} {:<8} {:<8} {:<7} {:<7} {:<5} {:<28} {}",
            row.id,
            row.missing,
            row.file_size,
            row.modified_at_unix,
            format_optional_f64(row.quality_score),
            row.ocr_status,
            embedding_label(row.has_image_embedding),
            embedding_label(row.has_ocr_text_embedding),
            row.face_count,
            trim_display(row.geo_label.as_deref().unwrap_or("-"), 28),
            row.path,
        );
    }

    Ok(())
}

fn format_optional_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{value:.3}"))
}

fn embedding_label(has_embedding: bool) -> &'static str {
    if has_embedding { "yes" } else { "-" }
}

fn trim_display(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let trimmed = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", trimmed.trim_end())
    } else {
        trimmed
    }
}
