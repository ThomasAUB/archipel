//! `archipel-cli`: the headless binary (PLAN §3.1). Not a user feature: it
//! proves the core meets its budgets independently of the UI, and gives
//! scripts a way to drive projects.

mod bench;
mod commands;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Archipel headless client.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Use this folder for config, data and caches (default: platform dirs,
    /// or `ARCHIPEL_HOME`).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the dashboard of a project file or of every repo below a folder.
    Dashboard {
        /// A `.gitproject.toml` or a folder to scan.
        target: PathBuf,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// List the repositories found below a folder.
    Scan {
        /// Folder to scan.
        root: PathBuf,
    },
    /// Fetch every repo of a project file or folder.
    Fetch {
        /// A `.gitproject.toml` or a folder to scan.
        target: PathBuf,
    },
    /// Print a window of the commit graph of a repository.
    Graph {
        /// Repository.
        repo: PathBuf,
        /// First row.
        #[arg(long, default_value_t = 0)]
        start: u32,
        /// Number of rows.
        #[arg(long, default_value_t = 30)]
        count: u32,
    },
    /// Update and print the statistics of a repository.
    Stats {
        /// Repository.
        repo: PathBuf,
    },
    /// Run the performance and memory benchmark (JSON lines on stdout).
    Bench {
        /// Reuse (or create) fixtures in this folder instead of a temp dir.
        #[arg(long)]
        fixtures: Option<PathBuf>,
        /// Exit with an error when a spec target is missed.
        #[arg(long)]
        check: bool,
        /// Smaller fixtures (quick smoke run).
        #[arg(long)]
        quick: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let dirs = match &cli.home {
        Some(h) => archipel_compose::AppDirs::under(h),
        None => archipel_compose::AppDirs::resolve()?,
    };
    match cli.command {
        Cmd::Dashboard { target, json } => commands::dashboard(dirs, &target, json),
        Cmd::Scan { root } => commands::scan(&root),
        Cmd::Fetch { target } => commands::fetch(dirs, &target),
        Cmd::Graph { repo, start, count } => commands::graph(dirs, &repo, start, count),
        Cmd::Stats { repo } => commands::stats(dirs, &repo),
        Cmd::Bench {
            fixtures,
            check,
            quick,
        } => bench::run(fixtures, check, quick),
    }
}
