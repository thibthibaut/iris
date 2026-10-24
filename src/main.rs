mod cli;
mod album;
mod config;
mod db;
mod image_io;
mod jobs;
mod render;
mod stages;
mod state;
mod web;

use anyhow::Result;
use clap::Parser;
use comfy_table::Table;
use iris_core::decisions::Decisions;
use tracing_subscriber::{EnvFilter, fmt};

use crate::{
    cli::{Cli, Command},
    config::Config,
    jobs::{CancelToken, StageCtx, TerminalSink, run_stage, stage_status},
};

fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("iris=info".parse()?))
        .init();

    let cli = Cli::parse();
    let config = Config::from_path(&cli.config)?;

    match cli.command {
        Command::Serve { host, port } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(web::serve(config, host, port))
        }
        Command::Stages => print_stages(&config),
        Command::Index => {
            let ctx = StageCtx::open(&config)?;
            let all = stages::all();
            let mut dirty: Vec<&str> = Vec::new();
            for stage in all.iter().filter(|s| !s.name().starts_with("export:")) {
                let status = stage_status(&ctx.db, &all, stage.as_ref(), &ctx.decisions)?;
                if status.stale || stage.deps().iter().any(|dep| dirty.contains(dep)) {
                    dirty.push(stage.name());
                    let ctx = StageCtx::open(&config)?;
                    run_stage(&ctx, stage.as_ref(), &TerminalSink::new(stage.name()), &CancelToken::default())?;
                }
            }
            print_stages(&config)
        }
        Command::Run { stages: names } => {
            let all = stages::all();
            for name in names {
                let stage = all
                    .iter()
                    .find(|stage| stage.name() == name)
                    .ok_or_else(|| anyhow::anyhow!("unknown stage {name}"))?;
                let ctx = StageCtx::open(&config)?;
                run_stage(&ctx, stage.as_ref(), &TerminalSink::new(stage.name()), &CancelToken::default())?;
            }
            Ok(())
        }
        Command::Inventory => {
            let ctx = StageCtx::open(&config)?;
            for column in ["source", "source_folder", "format", "camera_model", "taken_at_source"] {
                let mut table = Table::new();
                table.set_header(vec![column, "photos"]);
                for (value, count) in ctx.db.count_by(column)? {
                    table.add_row(vec![value, count.to_string()]);
                }
                println!("{table}");
            }
            println!("exact duplicate groups: {}", ctx.db.exact_duplicates()?.len());
            Ok(())
        }
    }
}

fn print_stages(config: &Config) -> Result<()> {
    let ctx = StageCtx::open(config)?;
    let decisions = Decisions::replay(&ctx.db.active_overrides()?);
    let all = stages::all();
    let mut table = Table::new();
    table.set_header(vec!["stage", "done", "failed", "pending", "stale"]);
    for stage in &all {
        let status = stage_status(&ctx.db, &all, stage.as_ref(), &decisions)?;
        table.add_row(vec![
            status.title.to_string(),
            status.done.to_string(),
            status.failed.to_string(),
            status.pending.to_string(),
            status.stale_reason.unwrap_or_default(),
        ]);
    }
    println!("{table}");
    Ok(())
}
