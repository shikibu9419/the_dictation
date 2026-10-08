# Qwenネイティブ推論コアの評価（2026-10-09）

## 今回の範囲と判断

`native/qwen-rs/` のRust＋MLX推論コアを実装し、現在のPython実装と同じモデル・同じPCMで比較した。
**Rust＋MLXで移行を続ける。** 日本語出力の一致と計算済みprefixの再利用を確認でき、通常の短音声の時間も同程度だったため。
ネイティブ化そのものによる高速化は確認できていない。37.86秒の入力ではPythonより約9%遅い。

これはアプリへの切替完了を意味しない。有限窓のlive制御、全文の区間処理、常駐JSONL worker、
resampler/発声開始ゲート、実行枠によるlive優先、モデル取得・app bundleへの組込みは未完了。
BLE・状態機械の変更も別段階で実装する。現行アプリはPython経路を使用している。

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

`native/qwen-rs/` で実行する。fixture生成やモデルパスの設定は同ディレクトリのREADMEを参照。

```sh
cargo test --release
cargo clippy --release --all-targets -- -D warnings
cargo test --release --lib short_tail -- --ignored --nocapture
cargo test --release --test runtime -- --ignored --nocapture
```

通常テストは数値処理・量子化・異常系を確認。モデルを使うignoredテストはJapanese tokenizer、
長音声の先頭・中間・末尾、長音声後の再録音、prefix再利用と未使用時のトークン一致、
cache reset、開始前と認識途中のcancel、その後の正常な再認識を確認する。

## 次の受入条件

- 最大30秒窓と日本語のprefix巻戻しを実装し、60秒以上のliveで消費位置と遅れを測る。
- 200msのPCM投入と1秒程度の推論間隔を分離する。受信確認と認識の消費位置を別々に返す。
- batchは全PCMを区間漏れなく扱い、生成上限を成功扱いしない。
- EOF/cancel/連続録音とlive/batch競合を、UIを出さないプロトコルテストで確認する。
- モデル取得とリソース同梱を移行してからPython起動経路を削除する。
