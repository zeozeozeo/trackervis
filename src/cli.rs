use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Tracker module oscilloscope visualizer and renderer",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
    #[command(flatten)]
    pub preview: PreviewArgs,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Render(RenderArgs),
    Preview(PreviewArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SortOrder {
    Filename,
    Mtime,
}

#[derive(Debug, Clone, Args)]
pub struct InputArgs {
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,
    #[arg(long, default_value = "filename")]
    pub sort: SortOrder,
    #[arg(long)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RenderArgs {
    #[command(flatten)]
    pub input: InputArgs,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long)]
    pub chapters: Option<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    #[arg(long, default_value_t = 60)]
    pub fps: u32,
    #[arg(long, default_value_t = 48_000)]
    pub sample_rate: u32,
    #[arg(long, default_value_t = 3_000)]
    pub history_ms: u32,
    #[arg(long, default_value_t = 240)]
    pub bins_per_second: u32,
    #[arg(long)]
    pub nvenc: bool,
    #[arg(long)]
    pub show_song_info: bool,
}

#[derive(Debug, Clone, Args)]
pub struct PreviewArgs {
    #[command(flatten)]
    pub input: InputArgs,
    #[arg(long, default_value_t = 3_000)]
    pub history_ms: u32,
    #[arg(long)]
    pub show_song_info: bool,
}
