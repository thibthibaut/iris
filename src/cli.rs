use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The wedding album: indexing, people, chapters, layout and print PDFs.
/// The app is the GUI (`iris serve`); these commands are thin wrappers for scripting.
#[derive(Debug, Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(long, default_value = "config/iris.toml", global = true)]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// The app: http://127.0.0.1:3000/
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3000)]
        port: u16,
    },
    /// Status of every stage.
    Stages,
    /// Runs every stale stage, in order.
    Index,
    /// Runs the given stages, in order.
    Run { stages: Vec<String> },
    /// Counts by source, folder, format, capture time; duplicates.
    Inventory,
}
