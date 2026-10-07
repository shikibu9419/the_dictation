mod adapters;
mod audio_file;
mod bluetooth;
mod capture;
mod collection;
mod config;
mod desktop_service;
mod filter;
mod gui;
mod helper;
mod model_download;
mod output;
mod pairing;
mod qwen_setup;
mod recognition;
mod recordings;
mod server;
mod settings;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use output::Output;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Index 01 BLE discovery and local speech to text")]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,
    #[arg(long, global = true)]
    log: Option<PathBuf>,
    #[arg(long, global = true, hide = true)]
    gui_events: bool,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Show the floating dictation panel and listen in the background.
    Gui,
    /// Download and verify the Whisper large-v3 model.
    DownloadModel,
    /// Install the isolated MLX runtime and Qwen3-ASR model.
    SetupQwen,
    /// Read or update persistent input and recognition settings.
    Settings {
        #[arg(long, value_enum)]
        input: Option<settings::InputSource>,
        #[arg(long, value_enum)]
        speech: Option<settings::SpeechModel>,
    },
    /// Hold right Option to dictate through the default Mac microphone.
    Microphone {
        #[arg(long, default_value = "ja-JP")]
        language: String,
    },
    /// Save the ring UUID; Bluetooth pairing happens when listening.
    Pair(Device),
    Scan(Scan),
    Inspect(Inspect),
    Listen(Listen),
    Fetch(Listen),
    Serve(Serve),
    Transcribe(Transcribe),
    /// Receive PCM events from an external microphone/input adapter.
    Stream {
        #[arg(long)]
        input_command: PathBuf,
        #[arg(long, default_value = "ja-JP")]
        language: String,
    },
}
#[derive(Args)]
pub struct Serve {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 8765)]
    port: u16,
    #[arg(long, default_value = "recordings")]
    output: PathBuf,
    #[arg(long, default_value = "ja-JP")]
    language: String,
}
#[derive(Args)]
pub struct Transcribe {
    file: PathBuf,
    #[arg(long)]
    raw_sample_rate: Option<u32>,
    #[arg(long, default_value = "ja-JP")]
    language: String,
    #[arg(long)]
    wav_output: Option<PathBuf>,
    #[arg(long)]
    text_output: Option<PathBuf>,
}
#[derive(Args)]
struct Scan {
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}
#[derive(Args)]
struct Device {
    address: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
}
#[derive(Args)]
struct Inspect {
    address: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    #[arg(long)]
    pair: bool,
}
#[derive(Args, Clone)]
pub struct Listen {
    address: Option<String>,
    #[arg(long, default_value_t = 30.0)]
    timeout: f64,
    #[arg(long, default_value_t = 0.25)]
    interval: f64,
    #[arg(long)]
    pair: bool,
    #[arg(long, conflicts_with = "no_transcribe")]
    transcribe: bool,
    #[arg(long)]
    no_transcribe: bool,
    #[arg(long, default_value = "ja-JP")]
    language: String,
}
impl Listen {
    fn transcription(&self, fetch: bool) -> bool {
        !self.no_transcribe && (self.transcribe || !fetch)
    }
}
fn arguments() -> Vec<String> {
    let mut args: Vec<String> = std::env::args().collect();
    let names = [
        "gui",
        "pair",
        "scan",
        "inspect",
        "listen",
        "fetch",
        "serve",
        "transcribe",
        "stream",
        "microphone",
        "download-model",
        "setup-qwen",
        "settings",
    ];
    let mut iter = args.iter().skip(1);
    let mut command = false;
    while let Some(value) = iter.next() {
        if ["--log", "--timeout", "--interval", "--language"].contains(&value.as_str()) {
            iter.next();
        } else if !value.starts_with('-') {
            command = names.contains(&value.as_str());
            break;
        }
    }
    if !command && !args.iter().any(|a| a == "-h" || a == "--help") {
        args.insert(1, "listen".into());
    }
    args
}
async fn run(action: Action, output: Output) -> Result<()> {
    let action = match action {
        Action::Stream {
            input_command,
            language,
        } => return adapters::input::stream::run(input_command, language, output).await,
        Action::Microphone { language } => {
            return adapters::input::stream::microphone(language, output).await;
        }
        Action::Settings { input, speech } => {
            let mut settings = settings::Settings::load()?;
            let changed = input.is_some() || speech.is_some();
            if let Some(input) = input {
                settings.input = input;
            }
            if let Some(speech) = speech {
                settings.speech = speech;
            }
            if changed {
                settings.save()?;
            }
            output.line(serde_json::to_string_pretty(&settings)?);
            return Ok(());
        }
        Action::DownloadModel => return model_download::download(output).await,
        Action::SetupQwen => return qwen_setup::setup(output).await,
        Action::Gui => return gui::run(output).await,
        Action::Serve(args) => return server::serve(args, output).await,
        Action::Transcribe(args) => return audio_file::transcribe(args, output).await,
        Action::Pair(args) => {
            let address = match args.address.as_ref() {
                Some(address) => Some(address.clone()),
                None => config::saved_address()?,
            };
            if let Some(address) = address {
                let address = pairing::normalize_address(&address)?;
                config::save_address(&address)?;
                output.error(format!("Device UUID saved: {address}"));
                return Ok(());
            }
            Action::Pair(args)
        }
        action => action,
    };
    let _lock = config::BluetoothLock::acquire()?;
    match action {
        Action::Listen(args) | Action::Fetch(args)
            if !args.timeout.is_finite()
                || args.timeout <= 0.0
                || !args.interval.is_finite()
                || args.interval <= 0.0 =>
        {
            anyhow::bail!("--timeout and --interval must be positive")
        }
        Action::Listen(args) => capture::run(args, false, output).await,
        Action::Fetch(args) => capture::run(args, true, output).await,
        other => {
            let seconds = match &other {
                Action::Pair(a) => a.timeout,
                Action::Scan(a) => a.timeout,
                Action::Inspect(a) => a.timeout,
                _ => unreachable!(),
            };
            ensure!(
                seconds.is_finite() && seconds > 0.0,
                "--timeout must be positive"
            );
            let mut ble = bluetooth::Bluetooth::start(output.clone()).await?;
            let result=async {match other {
                Action::Scan(_)=>{let devices=ble.request(json!({"type":"scan"}),seconds).await?;output.line(serde_json::to_string_pretty(&devices)?);}
                Action::Pair(args)=>{
                    let saved=if args.address.is_none() {config::saved_address()?} else {None};
                    let address=pairing::pair(&mut ble,args.address,saved,seconds,&output).await?;
                    config::save_address(&address)?;output.error(format!("Device UUID saved: {address}"));
                }
                Action::Inspect(args)=>{
                    let address=args.address.map(Ok).unwrap_or_else(config::load_address)?;
                    let found=ble.request(json!({"type":"find","address":address}),seconds).await?;
                    ensure!(!found.is_null(),"Device not advertising; press the Index button and retry scan");
                    ble.connect(&address,args.pair,seconds).await?;
                    let services=ble.request(json!({"type":"inspect"}),seconds).await?;
                    output.line(serde_json::to_string_pretty(&json!({"address":address,"bond_trigger_written":args.pair,"services":services}))?);
                }
                _=>unreachable!(),
            }Ok(())}.await;
            ble.close().await;
            result
        }
    }
}
#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).is_some_and(|a| a == "__whisper") {
        let result = (|| -> Result<()> {
            adapters::speech::whisper::worker(
                std::path::Path::new(raw.get(2).context("Missing model path")?),
                raw.get(3).context("Missing language")?,
                raw.get(4).context("Missing mode")?,
            )
        })();
        if let Err(error) = result {
            recognition::emit(json!({"type":"error","text":format!("{error:#}")}));
            std::process::exit(1);
        }
        return;
    }
    if raw.get(1).is_some_and(|a| a == "__worker") {
        let result = async {
            let options = serde_json::from_str(raw.get(2).context("Missing worker options")?)?;
            recognition::worker(options).await
        }
        .await;
        if let Err(error) = result {
            recognition::emit(json!({"type":"error","text":format!("{error:#}")}));
            std::process::exit(1);
        }
        return;
    }
    let cli = Cli::parse_from(arguments());
    let Some(action) = cli.command else { return };
    let mut output = match Output::new(cli.verbose, cli.log.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
        }
    };
    output.events = cli.gui_events;
    output.debug(format!("pebble-index start pid={}", std::process::id()));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    let parent_closed = async {
        if !cli.gui_events {
            std::future::pending::<()>().await;
        }
        let result = desktop_service::run(output.clone()).await;
        if let Err(error) = result {
            desktop_service::error(&output, &error);
            output.error(format!("Desktop service failed: {error:#}"));
        }
    };
    let result = tokio::select! { result=run(action,output.clone())=>result,_=tokio::signal::ctrl_c()=>Ok(()),_=terminate.recv()=>Ok(()),_=parent_closed=>Ok(()) };
    output.close();
    if let Err(error) = &result {
        output.event(&json!({"type":"error","text":format!("{error:#}")}));
        output.error(format!("Error: {error:#}"));
    }
    output.debug(format!("pebble-index stop pid={}", std::process::id()));
    if result.is_err() {
        std::process::exit(1);
    }
}
