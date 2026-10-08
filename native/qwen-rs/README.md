# Native Qwen inference

Rust library for Qwen3-ASR 1.7B on Apple Silicon, using the MLX C API and Metal.
This crate currently implements the inference core and headless evaluation.
The application's Python adapter has **not yet been replaced**. The JSONL worker,
bounded live session, model installer, and app-bundle integration are subsequent steps.

The fixed checkpoint is `moona3k/mlx-qwen3-asr-1.7b-8bit`, revision
`22c8abe6a6772122dda5905967d7496d1d3e8dd2`. Packed 8-bit/group-64 encoder,
embedding, decoder, and output weights are used directly. No Python or libtorch
is used by the Rust inference library. Model files are not in the repository.

## Build and numerical tests

Requires Apple Silicon macOS, Rust, CMake, Xcode and its Metal toolchain.
From this directory:

```sh
git submodule update --init
cargo test --release
cargo clippy --release --all-targets -- -D warnings
```

The first build fetches the pinned MLX source and compiles the native libraries.
The unit test covers periodic Hann/STFT/mel reference values, float16 activations,
packed quantized linear/embedding operations, scalar/data access, and error handling.
MLX arrays and compiled closures stay on their inference thread.

## Explicit model tests (no windows or playback)

Reuse the existing model without changing or deleting the Python environment:

```sh
export INDEX_QWEN_MODEL="$HOME/Library/Application Support/Index Voice/qwen-mlx/model"
export INDEX_QWEN_FIXTURES="$(mktemp -d)"
sh examples/make-fixtures.sh "$INDEX_QWEN_FIXTURES"
cargo test --release --lib short_tail -- --ignored --nocapture
cargo test --release --test runtime -- --ignored --nocapture
cargo run --release --example evaluate -- "$INDEX_QWEN_MODEL" \
  "$INDEX_QWEN_FIXTURES/short.wav" "$INDEX_QWEN_FIXTURES/long.wav" \
  "$INDEX_QWEN_FIXTURES/short.wav"
```

Runtime tests check Japanese tokenizer IDs, complete long-recording coverage,
repeated recordings, cancellation, and cached/uncached token agreement. The encoder
test compares whole-input and windowed execution, including a short final conv
chunk whose padding depends on preceding full chunks.

`WindowCache` belongs to one append-only live window. Reset it on window shifts,
new recordings, or language changes. It reuses complete 800-frame encoder blocks
and the decoder KV prefix, invalidating when log-mel normalization changes. Audio
input is mono float32 at 16 kHz; resampling belongs to the subsequent worker layer.

`examples/reference-python.py` is an optional **development comparison** against
`mlx-qwen3-asr 0.4.4`; the Rust crate does not invoke it. Benchmarks and limitations:
[Qwen native evaluation](../../docs/qwen-native-evaluation.md).

See `NOTICE`, `LICENSE`, `MLX-LICENSE`, and `vendor/mlx-c/LICENSE` for attribution.
