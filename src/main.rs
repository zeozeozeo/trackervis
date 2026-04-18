#[cfg(not(target_arch = "wasm32"))]
mod chapters;
#[cfg(not(target_arch = "wasm32"))]
mod cli;
#[cfg(not(target_arch = "wasm32"))]
mod discover;
#[cfg(not(target_arch = "wasm32"))]
mod export;
#[cfg(not(target_arch = "wasm32"))]
mod openmpt;
#[cfg(not(target_arch = "wasm32"))]
mod oscilloscope;
#[cfg(not(target_arch = "wasm32"))]
mod playlist;
#[cfg(not(target_arch = "wasm32"))]
mod preview;
#[cfg(not(target_arch = "wasm32"))]
mod render_host;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;
#[cfg(not(target_arch = "wasm32"))]
mod visualizer;

use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<()> {
    runtime::init_dll_search_path();

    let cli = cli::Cli::parse();
    match cli.command {
        Some(cli::Commands::Render(args)) => export::run(args),
        Some(cli::Commands::Preview(args)) => preview::run(args),
        None => preview::run(cli.preview),
    }
}

#[cfg(target_arch = "wasm32")]
fn main() -> Result<()> {
    anyhow::bail!(
        "the browser build is not implemented in this binary yet; split the app into a native CLI and a wasm frontend"
    )
}
