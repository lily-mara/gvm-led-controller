use std::sync::{Arc, Mutex};

use clap::Parser;
use tracing_subscriber::EnvFilter;

mod bluetooth;
mod gui;
mod protocol;

/// GUI for controlling GVM studio LEDs
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Fake the bluetooth stack
    #[arg(long)]
    fake: bool,
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let lights = Arc::new(Mutex::new(Vec::new()));

    let rt = tokio::runtime::Runtime::new()?;

    if args.fake {
        rt.spawn(bluetooth::fake_scan_and_spawn(lights.clone()));
    } else {
        rt.spawn(bluetooth::scan_and_spawn(lights.clone()));
    }

    gui::run(lights)?;

    Ok(())
}
