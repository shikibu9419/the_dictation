//! `listen`/`fetch`: receive from the ring and feed the recognition worker.
use crate::{
    Listen,
    recognition::{Client, Options, display_event},
    settings::Settings,
};
use anyhow::{Context, Result};
use pebble_core::{config, output::Output};
use pebble_ring::capture::{CaptureOptions, receive};

pub async fn run(args: Listen, fetch: bool, output: Output) -> Result<()> {
    let address = args
        .address
        .clone()
        .map(Ok)
        .unwrap_or_else(config::load_address)?;
    let interval = match args.interval {
        Some(interval) => interval,
        None => Settings::load()?.reception.state_poll_interval_ms as f64 / 1000.,
    };
    let mut speech = if args.transcription(fetch) {
        Some(
            Client::start(
                Options {
                    address: address.clone(),
                    language: args.language.clone(),
                    command: if fetch { "fetch" } else { "listen" }.into(),
                    verbose: output.verbose,
                },
                output.clone(),
            )
            .await?,
        )
    } else {
        None
    };
    let outbound = speech.as_ref().map(|s| s.outbound.clone());
    let options = CaptureOptions {
        address,
        timeout: args.timeout,
        interval,
        pair: args.pair,
        fetch,
    };
    let receiving = receive(options, outbound, output.clone());
    tokio::pin!(receiving);
    loop {
        if let Some(speech) = &mut speech {
            tokio::select! {
                result=&mut receiving=>{result?;speech.flush(&output).await?;return Ok(());}
                event=speech.events.recv()=>{display_event(event?.context("Recognition worker closed")??,&output)?;}
            }
        } else {
            return receiving.await;
        }
    }
}
