mod playback;
mod resample;
mod session;
mod settings;
mod tools;
mod view;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use pebble_core::{config, output::Output};
use pebble_ring::{bluetooth::Bluetooth, pairing};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Talk to the OpenAI Realtime API with the Index 01 ring button")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,
    #[arg(long, global = true)]
    log: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Save the ring UUID shared with the dictation app.
    Pair {
        address: Option<String>,
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Action::Pair { address, timeout }) => {
            ensure!(
                timeout.is_finite() && timeout > 0.0,
                "--timeout must be positive"
            );
            let output = Output::new(cli.verbose, cli.log.as_deref())?;
            tokio::runtime::Runtime::new()?.block_on(pair(address, timeout, output))
        }
        None => view::run(cli.verbose, cli.log),
    }
}

async fn pair(address: Option<String>, timeout: f64, output: Output) -> Result<()> {
    if let Some(address) = address {
        let address = pairing::normalize_address(&address)?;
        config::save_address(&address)?;
        output.line(format!("Device UUID saved: {address}"));
        return Ok(());
    }
    let _lock = config::BluetoothLock::acquire()?;
    let mut ble = Bluetooth::start(output.clone()).await?;
    let result = pairing::pair(&mut ble, None, config::saved_address()?, timeout, &output).await;
    ble.close().await;
    let address = result?;
    config::save_address(&address)?;
    output.line(format!("Device UUID saved: {address}"));
    Ok(())
}
