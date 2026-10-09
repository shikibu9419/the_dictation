use anyhow::{Context, Result};
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model = args
        .get(1)
        .context("usage: QwenNative MODEL_DIRECTORY LANGUAGE live|batch")?;
    let language = args.get(2).context("Missing language")?;
    let mode = args.get(3).context("Missing live|batch mode")?;
    qwen_asr::qwen::worker::run(std::path::Path::new(model), language, mode)
}
