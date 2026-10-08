# Native Qwen inference

Rust library for Qwen3-ASR 1.7B on Apple Silicon, using the MLX C API and Metal.
This crate implements the inference core and a resident JSONL worker, `QwenNative`.
The application invokes this worker directly. `setup-qwen` verifies the pinned model;
`build-app.sh` bundles the executable and `mlx.metallib`. Parent scheduling is a
separate implementation step.

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
It also places `mlx.metallib` in the profile directory alongside `QwenNative`; keep
these two files together when distributing the worker.
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
cargo test --release --test worker_runtime -- --ignored --nocapture --test-threads=1
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
input is mono float32 at 16 kHz; the worker resamples incoming PCM with continuous phase.

`examples/reference-python.py` is an optional **development comparison** against
`mlx-qwen3-asr 0.4.4`; the Rust crate does not invoke it. Benchmarks and limitations:
[Qwen native evaluation](../../docs/qwen-native-evaluation.md).

See `NOTICE`, `LICENSE`, `MLX-LICENSE`, and `vendor/mlx-c/LICENSE` for attribution.

## Worker protocol

Run `target/release/QwenNative MODEL_DIRECTORY ja_JP live` (or `batch`).
Stdin and stdout use JSONL. Stdout contains protocol messages only.

- `audio`: base64 little-endian signed PCM16, `sample_rate`, `session_id`, `generation`.
- `finish`: same session identifiers, after all PCM has been sent.
- `cancel`: invalidates queued and computing results immediately; drops recording state
  on the inference thread at the next computation boundary.
- `permit`: `enabled: false` suspends inference at an encoder/token boundary;
  `true` resumes the same work. PCM reception and acknowledgement remain active.
- EOF terminates the worker, including while paused. The model is loaded once.

Replies carry `protocol_version: 2`. `accepted_samples` counts received source PCM;
`consumed_samples` counts the source PCM incorporated into a recognition update.
The latter is monotonic within a recording. An `accepted` response is not a transcript.
Session replies echo the identifiers. Legacy clients may omit both identifiers.
A queue over 300 seconds or a protocol line over 16 MiB fails explicitly.

Live recognition uses a 1-second inference cadence and at most 30 seconds of PCM,
independently of packet size. Completed windows reuse stable encoder/text prefixes;
window shifts reset caches. Batch recognition covers disjoint audio intervals from
start to finish. Segment boundaries use sustained quiet near the end of the window;
no Japanese substring deduplication is applied. Entire-recording final recognition
is a separate batch request, rather than the live worker's `finish` result.
