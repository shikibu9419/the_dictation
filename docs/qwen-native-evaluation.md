# Qwenネイティブ推論コアの評価（2026-10-09）

## 今回の範囲と判断

`src/qwen/` のRust＋MLX推論コアを実装し、現在のPython実装と同じモデル・同じPCMで比較した。
**Rust＋MLXで移行を続ける。** 日本語出力の一致と計算済みprefixの再利用を確認でき、通常の短音声の時間も同程度だったため。
ネイティブ化そのものによる高速化は確認できていない。37.86秒の入力ではPythonより約9%遅い。

これはアプリへの切替完了を意味しない。有限窓のlive制御、全文の区間処理、常駐JSONL worker、
resampler/発声開始ゲートは追加した。モデル取得・app bundleへの組込みも移行した。親の実行枠によるlive優先も実装・検証した。
BLE・状態機械の変更も別段階で実装する。現在のソースはネイティブworkerを呼び出す。

## 実装と互換修正

- `second-state/qwen3_asr_rs` のモデルとMLXラッパーを基に、libtorch、HTTPサーバー、音声ファイルデコーダーを取り込まず独立crate化。
- 既存のpacked 8bit/group-64重みを直接読む。encoder、embedding、decoder、lm_headの量子化に対応。
- periodic Hann、反射padding、自然長のSTFTと末尾1フレーム除去をPython版に揃え、FFTをまとめて実行。
- GELUを近似式から元モデルの式へ修正。定数による不要なFloat32化を防ぎ、GELU/SiLUをMLXで融合。
- conv末尾は、同一音声にfull chunkがある場合にpaddingする。encoderのattention windowへ分割した後もこの条件を維持。
- 完了した800 melフレームごとのencoder区間と、それに対応するdecoder KV prefixを再利用。
  末尾の反射paddingが変化する区間はキャッシュせず、log-mel最大値が変われば無効化する。
- prefillで全位置を語彙へ射影せず、次トークンに必要な末尾位置のみを計算。
- encoder窓・生成トークンの境界でキャンセルを確認。新しい録音は新しいcacheで開始する。
- C APIの演算戻り値を検査し、MLXの標準エラーハンドラによるstdoutへの書込みを除去。
  MLX arrayの無条件Send/Syncを除去し、streamとコンパイル済みclosureをスレッドに閉じ込めた。

## 環境・比較方法

- Apple M4、32 GB、macOS 26.0.1、Rust 1.92.0。
- Rust: mlx-c `a1290d221f92bd020af805b7d14207eee4ec973b`、MLX `185b06d9efc1c869540eccfb5baff853fff3659d`（0.30.6）。
- Python: `mlx-qwen3-asr 0.4.4`、MLX 0.31.2。手元の既存環境を使用。
- 両方とも `moona3k/mlx-qwen3-asr-1.7b-8bit`、revision `22c8abe6a6772122dda5905967d7496d1d3e8dd2`、言語Japanese。
- macOS Kyoko音声をファイルへ生成し、mono/16 kHz/PCM16へ変換。再生、GUI起動、BLE接続は行っていない。
- 各実装を別々に実行。モデルを常駐させ、初回短音声→短音声3回→長音声3回→短音声3回の順で測定。
- 表の時間は認識API呼出しから結果まで。ファイル読込とmodelオブジェクト生成は含まない。
  初回は遅延評価される重み読込などを含む。ウォーム3回の中央値を使用。
- Pythonのbatch APIは長音声を内部で分割する。今回のRustコアは同じ37.86秒全体をdecoderへ渡している。
  後続の区間処理を入れた状態でも再評価が必要。

| 入力 | Rust | Python | Rust RTF |
| --- | ---: | ---: | ---: |
| 4.14秒、初回1回 | 0.766秒 | 0.529秒 | 0.185 |
| 4.14秒、ウォーム中央値 | 0.416秒 | 0.414秒 | 0.100 |
| 37.86秒、中央値 | 4.564秒 | 4.204秒 | 0.121 |
| 長音声の後の4.14秒、中央値 | 0.435秒 | 0.450秒 | 0.105 |

全10組で出力文が一致。TTSへ渡した文章に対するCERは両方とも短音声0%、長音声3.43%だった。
長音声の差は句読点と「時／とき」などを含む。これは2種類の合成音声に対する結果であり、
指輪の音声や一般的な日本語精度の評価にはならない。測定時はユーザーの他作業を停止していない。
生の各測定値・文字列・参照文は [qwen-native-benchmark.json](qwen-native-benchmark.json) に保存。

## prefix再利用と数値検証

9.1秒の入力のうち、直前の8.1秒入力から完了済み8秒（800 mel frames、113 decoder positions）を再利用した。

- 再利用あり0.646秒、なし0.829秒。同じ出力トークン列を確認（別測定では0.665秒／0.862秒）。
- この比較ではtext prefixを与えていないため、生成する全文トークンの時間が大半を占める。
  liveでのtext prefix巻戻しと有限窓は後続実装で評価する。
- 同じ4.14秒音声のmel全体についてPythonとの最大差 `3.58e-7`、平均差 `5.35e-8`。
- encoderの全体実行と窓別実行は810/899/901フレームで比較。最大差 `4.12e-4` 以下、RMS差 `1.59e-5` 以下。
  paddingを意図的に省くと許容差を超えることも回帰テストで確認した。

## 検証コマンド

`desktop/rust/` で実行する。fixture生成やモデルパスの設定は [Qwen実装](qwen.md) を参照。

```sh
cargo test --release --lib qwen::
cargo test --release --test qwen_numerics --test qwen_worker_runtime
cargo clippy --release --all-targets -- -D warnings
cargo test --release --lib short_tail -- --ignored --nocapture
cargo test --release --test qwen_runtime -- --ignored --nocapture
```

通常テストは数値処理・量子化・異常系を確認。モデルを使うignoredテストはJapanese tokenizer、
長音声の先頭・中間・末尾、長音声後の再録音、prefix再利用と未使用時のトークン一致、
cache reset、開始前と認識途中のcancel、その後の正常な再認識を確認する。

## 常駐ワーカーの評価

`QwenNative` に200 msずつPCMを投入し、75.712秒（上記の長音声を2回連結）のliveを測定。
音声の再生、マイク、BLE、GUIは使用していない。1秒ごとに推論し、30秒窓で処理する。

| 指標 | 結果 |
| --- | ---: |
| 最初のPCM投入開始基準の初回partial | 1.202秒 |
| 前半20秒までの認識待ち中央値 | 0.4秒 |
| 後半55秒以降の認識待ち中央値 | 0.4秒 |
| 認識待ち最大値 | 2.2秒 |
| 入力開始からfinalまで | 77.306秒 |
| MLX peak memory | 2,609,368,216 bytes |

認識待ちは、応答時点の `(accepted_samples - consumed_samples) / sample_rate`。
実際の発話からUI表示までの時間ではない。MLXのメモリ指標はプロセス全体のRSSではない。
窓確定時の全文再生成をやめ、安定したprefixを再利用することで、初期版の最大6.275秒から短縮した。
消費位置は単調増加を検査し、最終値が全入力サンプル数と一致することを確認した。
長音声後の短音声も冒頭と同じ結果で、前録音の文章が混ざらない。
同じ文章が2回含まれる試料の先頭・末尾を確認したが、liveの一般的な日本語精度を保証する試験ではない。

通常テスト14件（13 unit + 1数値テスト）とworker実モデルテスト3件が通過。
追加テストはpaused batchへの全PCM受領、cancelによる旧結果破棄、resume後の全文認識、
無音の空結果、連続録音、EOF正常終了、任意のPCMパケット境界でのリサンプル位相保持、
長い無音末尾で認識済み本文を消さないこと、全区間の連続性を確認する。
実行コマンド: `cargo test --release --test qwen_worker_runtime -- --ignored --nocapture --test-threads=1`。

## アプリ接続と配布の確認

- `ProcessEngine` は直接 `QwenNative` を起動。受信結果のversion・録音ID・世代・消費位置を検査し、キャンセル送信時点から旧結果を破棄する。外部アダプターは既存JSONLのまま使用できる。
- `setup-qwen` はRustでSHA-256を確認し、不足ファイルだけcurlで取得する。旧Python/uvアダプターを削除。既存のモデルや旧venvは削除しない。
- 実モデルを一時ディレクトリから参照し、セットアップ2回、連続認識2回、cancel後の空final、EOF終了をアダプター経由で確認した。ユーザーの設定・保存モデルは変更していない。
- 一時ディレクトリへappをビルド。`QwenNative` と `mlx.metallib`、ライセンスの同梱、Metalリソースを含む署名、`codesign --verify --deep --strict`を確認した。`otool -L`で依存先はmacOSのシステムライブラリのみ。アプリは起動していない。
- 親の通常unit 88件、外部アダプター/CLI互換テスト7件が通過。`cargo clippy --all-targets -- -D warnings`も通過。
- 署名済みapp内の `QwenNative` を指定したアダプターの実モデルテストも通過。モデル検証2回を含むreleaseテストは23.02秒。GUI、BLE、マイクは起動していない。
- ワーカーの測定値は [qwen-native-worker-benchmark.json](qwen-native-worker-benchmark.json) に保存した。

## live優先と中断・再開の評価

Qwen同士を常駐させ、37.856秒のbatchが実際に推論を開始してからliveを2回開始した。
`permit`の受領だけでは次のモデルを動かさず、モデル所有スレッドがencoder/tokenの境界で
Metalを同期し、`paused: true`を返してからliveへ実行枠を渡す。
録音IDとは独立した要求番号により、古い停止確認を新しい要求へ流用しない。

| 条件 | 実行枠取得後の初回partial | 停止確認まで | PCM遅れ最大 | live finish後final |
| --- | ---: | ---: | ---: | ---: |
| batchなし、最初のlive | 2.202秒 | — | 1.0秒 | 0.797秒 |
| batchを止めたlive 1回目 | 1.202秒 | 0.493秒 | 0.4秒 | 0.700秒 |
| batchを止めたlive 2回目 | 1.403秒 | 0.120秒 | 0.2秒 | 0.666秒 |

3回とも同じ全文結果。停止確認時間を含めると後2回の初回partialは約1.695秒、1.523秒。
順序・カーネルの初回準備・他作業の影響があるため、batchがある方が高速とは判断しない。
別の試行では停止確認0.154秒/0.662秒、初回partial1.603秒/1.401秒だった。
停止は瞬時ではなく、現在の計算単位を終えるまで待つ。

liveが実行枠を持つ間はbatchが計算結果を出さないことを確認。終了後は同じbatchを再開し、
`0..27.525..37.8561875`秒を欠落・重複なく覆い、冒頭・末尾の文章を確認した。
liveのMLX peak memoryは2,268,914,354 bytes。両ワーカー合計RSSではない。
測定値は [qwen-native-priority-benchmark.json](qwen-native-priority-benchmark.json) に保存。

- 親は受信確認と消費位置を区別し、未消費PCMが60秒（2窓）に達したら追加投入を待つ。残りは親が保持する。停止中でもBLE受信とPCM保持は別の処理で続ける。
- 意図的な停止時間はbatchの認識タイムアウトから除外。EOF/エラーは停止中も処理する。
- `Helper`のstdout監視が停止確認を受け取るため、通常の結果取得がfinal待ちでも実行制御は止まらない。共有stdinもcloseで実体をdropする。
- 親の通常テスト100件、nativeの通常テスト15件が通過。停止確認前のEOF、受領と計算停止の区別、タイムアウトの停止時間除外、2窓の投入上限を含む。
- 実モデルの優先制御試験1件、nativeのpause/cancel/連続録音/EOF試験2件が通過。両crateのclippyも通過。

## 残る受入条件

- 認識ワーカーの異常時に、親が保持するPCMを使って対象ワーカーだけを再起動・再送する。
- BLEの音声保持と状態管理の変更を接続し、入力から認識までを別途確認する。


## ルートCargoへの統合（2026-10-09）

ユーザー指定に合わせ、推論実装を `src/qwen/`、workerの入口を
`src/bin/qwen_native.rs` へ移動した。Cargoパッケージとlockfileはルートの1組に統一。
C/C++依存は固定revisionの `vendor/mlx-c`、ライセンスは `licenses/qwen/` に置く。
プロセス分離・モデル・PCM/JSONLプロトコル・推論方式は維持した。

- Qwenのunit 14件、数値1件、ルートで生成したworker/Metalリソースの配置1件が通過。
- `cargo clippy --release --all-targets -- -D warnings` が通過。
- 画面なしのCLI・外部入力adapter・表示モデルのテストも実行した。
- ルートの `build-app.sh` で一時ディレクトリへappを生成し、同梱・署名を確認。
  `codesign --verify --deep --strict` は成功。QwenNativeの動的リンク先はmacOSのシステムライブラリだけ。
- 同梱workerを親adapterから起動し、セットアップ再利用・連続認識・cancel・EOF、
  liveが計算中のbatchを中断して同じ録音へ復帰する実モデルテスト2件が通過。
- worker単体のbatch/cancel/EOFの実モデルテスト2件と、75.712秒のpaced liveが通過。
  ユーザーのアプリ起動・BLE・マイク・画面表示・音声再生は行っていない。

| 75.712秒入力 | コンパイルと同時 | 当方のビルド終了後 |
| --- | ---: | ---: |
| 初回partial | 1.425秒 | 1.255秒 |
| 前半の認識待ち中央値 | 1.0秒 | 0.4秒 |
| 後半の認識待ち中央値 | 1.0秒 | 1.2秒 |
| 認識待ち最大 | 7.8秒 | 4.2秒 |
| 開始からfinal | 77.144秒 | 76.840秒 |

ビルド終了後もユーザーの他作業は継続している。以前の最大2.2秒より大きく、
各1回の結果から配置変更の影響、温度、他の負荷を切り分けることはできない。
入力全体の消費・全文の先頭と末尾・窓とメモリの上限は確認したが、
**遅延が従来と同じ、または短縮したという証拠にはしない。**
生データは [Cargo統合後の測定値](qwen-package-integration-benchmark.json) に保存。
