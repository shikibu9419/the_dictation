# Repository Guidelines

## Project Structure & Module Organization

This standalone Rust 2024 Cargo workspace provides local Pebble Index 01 tooling on macOS. Applications live in `apps/`, shared crates in `crates/`:

- `apps/dictation`: the dictation app. `src/main.rs` defines the `pebble-index` CLI; `src/overlay/` implements the `index-voice` GPUI interface. Speech backends live in `src/adapters/speech/`; other `src/` modules handle recognition, settings and HTTP serving. `native/` holds its Swift helpers; `tests/` its integration tests; `docs/` architecture notes.
- `apps/pebble-jev`: the ring-to-OpenAI Realtime API app (`pebble-jev` binary).
- `crates/core` (`pebble-core`): logging, JSONL IPC, Swift helper processes, config directory, PCM utilities.
- `crates/pebble-ring`: Bluetooth reception, collection decoding, the button/gesture reducer (`reception/`) and the PCM input boundary (`input/`). `native/Bluetooth.swift` lives here.
- `crates/ui` (`pebble-ui`): presentation-only GPUI components and the macOS floating panel bridge (`native/OverlayMac.m`, compiled by its `build.rs`).
- `crates/openai-realtime`: OpenAI Realtime API WebSocket client.
- `crates/qwen-asr`: Qwen3-ASR on MLX and the `QwenNative` worker. Only this crate builds `vendor/mlx-c`.

Unit tests live next to the code; integration tests live in each package's `tests/`. Build outputs and generated app bundles belong in the root `target/`.

## Build, Test, and Development Commands

Use macOS 26+, Xcode 26+, Rust 1.92+, and CMake (only for `qwen-asr`). Run commands from this directory:

- `cargo build --release -p dictation -p qwen-asr --bins`: build the dictation executables.
- `cargo run --release -p dictation -- pair`: discover and save the ring UUID (shared by both apps).
- `cargo run --release -p dictation -- gui`: launch the floating dictation interface.
- `cargo run --release -p pebble-jev`: launch the Realtime API app.
- `sh apps/dictation/build-app.sh` / `sh apps/pebble-jev/build-app.sh`: build and sign the app bundles into `target/`.
- `cargo test --workspace`: run the default test suite.
- `cargo clippy --workspace --all-targets -- -D warnings`: check all targets and reject warnings.
- `cargo fmt --all --check`: check Rust formatting; use `cargo fmt --all` to apply it.

## Coding Style & Naming Conventions

Use standard rustfmt formatting and four-space indentation. Name modules, functions, and tests in `snake_case`, types in `UpperCamelCase`, and constants in `SCREAMING_SNAKE_CASE`. Keep `crates/ui` components presentation-only: they take props and callbacks, never app state. Keep app policy (settings, recognition, tools) in `apps/`; put code in `crates/` only when a second app consumes it. Keep platform-specific code in native helpers or dedicated bridge modules.

## Testing Guidelines

Use Rust's built-in test framework and `#[tokio::test]` for asynchronous tests. Give tests descriptive behavior-based names. Isolate configuration and generated audio with temporary directories and `XDG_CONFIG_HOME`.

Real-runtime tests are explicitly ignored by default. For example, run `cargo test -p dictation --test speech_runtime -- --ignored --nocapture` on a compatible Mac. Whisper tests require downloaded models; Bluetooth tests perform real scans. No numeric coverage threshold is configured; add regression tests for changed behavior.

## Commit & Pull Request Guidelines

Recent commits use imperative subjects such as “Separate live and batch recognition plans.” Follow that style and keep commits focused. PRs should explain the behavior change, link relevant issues, and report validation commands and untested hardware paths. Include screenshots for visible GUI changes and update `README.md` when commands or settings change.
