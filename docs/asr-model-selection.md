# On Deviceモデルの選定（2026-10-02）

Qwen3-ASR 1.7Bの8bit MLX版を採用。設定画面はSpeechAnalyzer / On Deviceの2択を維持する。

## 日本語の公開評価

| モデル | FLEURS日本語 CER | 条件 |
|---|---:|---|
| Qwen3-ASR 1.7B | 5.20% | 開発元のオフライン評価 |
| Voxtral Realtime | 9.59% | 開発元の480ms遅延設定 |
| Voxtral Realtime | 6.80% | 同960ms |
| Voxtral Realtime | 5.50% | 同2400ms |

出典: [Qwen論文 Table A.2(b)](https://arxiv.org/html/2601.21337v2)、[Voxtral論文 Table 7](https://arxiv.org/html/2602.11298v1)。

異なる実装・正規化・認識モードの評価なので、同条件の直接比較ではない。Qwenの5.20%を日本語ストリーミング精度として扱わない。別の[公開実測650件](https://github.com/kuro-shiba/ASR_ja_comparison/blob/main/asr/fleurs_ja/results/fleurs-ja-650-summary-pass1-no-whisper-large-v3.md)ではQwen 1.7Bは6.06%だった。これもリング音声や8bit MLXアダプターの評価ではない。

精度優先の用途、録音終了後の全音声再認識、モデル規模1.7Bという条件からQwenを選ぶ。Voxtralはネイティブな連続入力とサブ秒遅延に利点がある。日本語の精度差を確定するには同一リング音声での比較が必要。

## Qwenはストリーミング可能か

可能。公式はvLLMで逐次音声入力を提供し、論文Table 8では2秒チャンク・末尾5トークンの巻き戻しで英語と中国語を評価している。日本語ストリーミングの数値は同表にはない。

mlx-audioのファイルに対する`stream_transcribe`は出力トークンのストリーミング。調査時点でライブPCM入力のPR #967は未マージで、全蓄積音声の再エンコード等の制約があるため採用しない。

採用する[mlx-qwen3-asr 0.4.4](https://github.com/moona3k/mlx-qwen3-asr)は`feed_audio`による逐次PCM入力に対応。公式方式に沿うテキストprefixの巻き戻し、最大30秒窓、確定部分の計算再利用、無音付近での窓確定がある。PyPI版streaming.pyは調査したコミット`41878a11cf338c59e13edc84bf4cb35f4b0f3ff6`の同ファイルとSHA-256一致を確認した。

ライブ更新は1秒単位。入力ACKは推論から分離し、キャンセル時は世代を更新して旧結果を破棄。全音声の再認識は独立したbatchプロセスで行う。リサンプルはパケット境界を跨いで位相を保持する。

## 依存とモデル

モデル: `moona3k/mlx-qwen3-asr-1.7b-8bit`、revision `22c8abe6a6772122dda5905967d7496d1d3e8dd2`。モデルと実装はApache-2.0。MLX、NumPy、regex、huggingface-hubとその依存のみ。PyTorch、Transformers、mlx-audio全体は不要。

## このMacでの動作確認

36.173秒の日本語合成音声を250msずつ実時間で入力し、その後同じプロセスで10秒音声を入力。ライブ・batch各2回、キャンセル後の空録音、標準入力EOFによる正常終了を確認した。

| 項目 | 1回目（36.173秒） | 2回目（10秒） |
|---|---:|---:|
| ライブ初回途中結果 | 1.286秒 | 1.024秒 |
| finish送信からライブ最終結果 | 0.480秒 | 0.209秒 |
| finish送信から独立した全文再認識の結果 | 5.304秒 | 1.407秒 |

36秒の録音で30秒窓を跨ぎ、冒頭の「最初の確認です」から末尾の「これで最後の確認を終わります」まで出力された。ライブの「持っ来てください」は全音声の再認識で「持ってきてください」になった。

これは合成音声による接続・逐次入力・状態初期化の確認であり、リング実機の精度評価やVoxtralとの直接比較ではない。ビルドを並行実行していたため速度の厳密なベンチマークでもない。
