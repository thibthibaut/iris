mod app;
mod capture;
mod cli;
mod config;
mod db;
mod hash;
mod image_io;
mod models;
mod processors;
mod text;
mod traits;
mod web;
mod webp_cache;

use anyhow::Result;
use clap::Parser;
use comfy_table::Table;
use tracing_subscriber::{EnvFilter, fmt};

use crate::app::AppContext;
use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::db::Database;
use crate::processors::{
    ClusterFacesProcessor, DiscoveryProcessor, FaceEmbeddingProcessor, ImageEmbeddingProcessor,
    LazyOcrProcessor, MetadataProcessor, OcrTextEmbeddingProcessor, QualityProcessor,
    ReverseGeoProcessor, WebpCacheProcessor, run_index,
};
use crate::traits::BatchProcessor;

fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("iris=info".parse()?))
        .init();

    let cli = Cli::parse();
    let config = Config::from_path(&cli.config)?;
    let db = Database::open(&config.database_path)?;
    let ctx = AppContext::new(config, db, cli.limit, cli.limit);

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
        Command::ClusterFaces => ClusterFacesProcessor.run(&ctx),
        Command::Ocr => LazyOcrProcessor.run(&ctx),
        Command::WebpCache => WebpCacheProcessor.run(&ctx),
        Command::ShowDb => show_db(&ctx),
        Command::Verify => verify(&ctx),
        Command::Serve { host, port } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(web::serve(ctx.config, host, port))
        }
        Command::Index => run_index(&ctx),
    }
}

fn show_db(ctx: &AppContext) -> Result<()> {
    let rows = ctx.db.photo_rows(20)?;
    let mut table = Table::new();
    table.set_header(vec![
        "id", "missing", "size", "mtime", "quality", "ocr", "imgvec", "txtvec", "faces",
        "geo", "path",
    ]);

    for row in rows {
        table.add_row(vec![
            row.id.to_string(),
            row.missing.to_string(),
            row.file_size.to_string(),
            row.modified_at_unix.to_string(),
            format_optional_f64(row.quality_score),
            row.ocr_status,
            embedding_label(row.has_image_embedding).to_string(),
            embedding_label(row.has_ocr_text_embedding).to_string(),
            row.face_count.to_string(),
            row.geo_label.unwrap_or_else(|| "-".to_string()),
            row.path,
        ]);
    }

    println!("{table}");
    Ok(())
}

fn verify(ctx: &AppContext) -> Result<()> {
    let photos = ctx.db.photos_for_verify(ctx.pending_limit)?;
    let total = photos.len();
    let pb = processors::progress::bar(total, "verify");
    let mut mismatches = 0;
    let mut failed = 0;

    for photo in photos {
        match hash::blake3_file(photo.path.as_ref()) {
            Ok(current_hash) => {
                if current_hash != photo.blake3_hash {
                    mismatches += 1;
                    println!(
                        "MISMATCH id={} path={} expected={} got={}",
                        photo.id, photo.path, photo.blake3_hash, current_hash
                    );
                }
            }
            Err(error) => {
                failed += 1;
                eprintln!("FAILED id={} path={} error={}", photo.id, photo.path, error);
            }
        }
        pb.inc(1);
    }

    pb.finish_and_clear();
    println!("verify completed: {total} checked, {mismatches} mismatches, {failed} failed");
    Ok(())
}

fn format_optional_f64(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{value:.3}"))
}

fn embedding_label(has_embedding: bool) -> &'static str {
    if has_embedding { "yes" } else { "-" }
}


