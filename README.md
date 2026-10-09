# Pebble Index 01 — Rust版（ワークスペース）

> **非公式・自己責任での利用について**  
> このソフトウェアは、Pebble Index 01をMacへ直接接続して使用する非公式の実装です。メーカーが提供する本来の利用方法とは異なります。使用・改変・開発は、ご自身の責任で行ってください。動作やデータの保全を保証するものではありません。

Index 01 リングを Mac から直接使う 2 つのアプリと、その共有クレートをまとめた Cargo ワークスペースです。

- **[dictation](apps/dictation/README.md)** — リングの音声を BLE で受信し、Apple SpeechAnalyzer か On Device（Qwen3-ASR MLX）で文字起こしして貼り付けるフローティング GUI と CLI（`index-voice` / `pebble-index`）。
- **[pebble-jev](apps/pebble-jev/README.md)** — リングのボタンを押している間の音声を OpenAI Realtime API に送り、応答音声・文字起こし・tool call（TODO／メモ追加）をストリーミング表示する会話アプリ。

この独立した `desktop/rust` リポジトリの独自実装は **Apache License 2.0** で提供します。[LICENSE](LICENSE) と [NOTICE](NOTICE) を参照してください。依存ライブラリ・モデル・Apple SDK等にはそれぞれのライセンス・利用条件が適用されます。親の `mobileapp` や他のリポジトリのライセンスを変更するものではありません。

## 必要なもの

- macOS 26以降（SpeechAnalyzerを使う場合はSpeechTranscriber対応Mac）
- Xcode 26以降（初回起動のセットアップを済ませ、Command Line Toolsに選択）
- Rust 1.92以降とCargo
- CMake（`qwen-asr` の MLX ビルド用。未導入なら `brew install cmake`。pebble-jev だけなら不要）
- Pebble Index 01（dictation は Mac のマイクでも動きます）

初回はCargo依存関係とAppleの音声モデルの取得にインターネット接続が必要です。Apple APIを呼ぶSwiftヘルパーは初回実行時にビルドし、以降はキャッシュを使います。SwiftソースはRustバイナリへ埋め込まれるため、実行時にこのリポジトリのソースを探す必要はありません。

## リポジトリ構成

Cargoワークスペースです。アプリは `apps/`、共有クレートは `crates/` にあります。

| パス | 内容 |
| --- | --- |
| `apps/dictation` | 音声入力アプリ（`pebble-index` CLI・`index-voice` GUI、[README](apps/dictation/README.md)） |
| `apps/pebble-jev` | リングのボタンでOpenAI Realtime APIと会話するアプリ（[README](apps/pebble-jev/README.md)） |
| `crates/core` | ログ・JSONL IPC・Swiftヘルパー起動・設定ディレクトリ・PCMユーティリティ |
| `crates/pebble-ring` | Index 01のBLE受信、転送データのデコード、ボタン状態機械、PCM入力境界 |
| `crates/ui` | 見た目だけを担当するGPUIコンポーネントとmacOSフローティングパネルの橋渡し |
| `crates/openai-realtime` | OpenAI Realtime APIのWebSocketクライアント |
| `crates/qwen-asr` | Qwen3-ASRのMLX推論と`QwenNative`ワーカー（mlx-cのビルドはここだけ） |

## 共通のビルド・検証

コマンドはすべてこのディレクトリ（ワークスペースのルート）で実行します。

```sh
git submodule update --init -- vendor/mlx-c      # dictation（qwen-asr）を使う場合のみ
cargo build --release -p dictation -p qwen-asr --bins
cargo build --release -p pebble-jev
sh apps/dictation/build-app.sh                   # target/Index Voice.app
sh apps/pebble-jev/build-app.sh                  # target/Pebble Jev.app

cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

リングの登録（`pair`）は両アプリで共有の `~/.config/pebble-index-rust/device.json` に保存され、同時接続は同じディレクトリのロックで排他します。どちらかのアプリが接続中なら、もう一方は終了してから起動してください。

リリースビルド（gpui・whisper・MLX）は数 GB の空き容量を使います。アプリごとの使い方・設定・検証は各 README を参照してください。
