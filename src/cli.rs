use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(long, default_value = "config/iris.toml", global = true)]
    pub config: PathBuf,

    #[arg(long, global = true)]
    pub limit: Option<usize>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Scan,
    Metadata,
    Geo,
    Quality,
    Embed,
    Faces,
    Ocr,
    WebpCache,
    ShowDb,
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long, default_value_t = 3000)]
        port: u16,
    },
    All,
}
