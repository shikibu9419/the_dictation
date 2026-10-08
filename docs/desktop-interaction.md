# デスクトップの受信・録音状態

## 責務

| 層 | 実装 | 所有するもの |
| --- | --- | --- |
| BLE取得 | `capture.rs / bluetooth.rs` | S/R/C要求、取得時刻とsequence、同じFIFOへ送るclock |
| 音声ストア | `recordings.rs / pcm.rs` | source ID、不変PCM、連続して取得済みの範囲、欠番とfinal |
| 正規化 | `reception/button_detector.rs` | 83の既出prefix、新規short/long、判定根拠 |
| 状態遷移 | `reception/session_state.rs` | sourceの所属、候補、UI/再開/タップ期限、snapshot |
| 音声投入計画 | `reception/input_effects.rs` | live cursor、共有PCMへの範囲参照、全文ジョブ、世代 |
| 入力アダプター | `adapters/input/index.rs / interaction.rs` | 上記の接続、source解放のACK、割当フック |
| 認識制御 | `recognition.rs` | live/batch、録音IDごとの表示許可・世代照合 |
| 表示 | `overlay/model.rs` | ID付き進捗、編集・非表示・履歴。独自の押下タイマーは持たない |

`gesture_types.rs` はフックに渡す値、`gestures.rs` は動作の割当口。音声やモデルに依存しない。

## 観測から録音へ

Sの `in_collection_state=true` で候補を作る。音声はsourceごとのストアへ保持し、表示待ち（初期値50ms）を過ぎても収集中ならrecording表示を出す。50ms経過はファームウェアのlong確定を意味しない。

falseまたは対応するfinalで終了猶予（初期値50ms）を始める。同じ終了情報で期限を延長しない。猶予後はdictatingへ移り、新しいlive入力を止めるが、音声回収は続ける。所属する全sourceのfinalと欠番なしを確認して全文認識を開始する。

未完了の同じsourceが再び収集中なら同じ録音へ戻る。別のfinal済みsourceとの結合は、猶予内の再開かつlong同士と確認した場合のみ。再開候補がshortなら本文へ混ぜず、必要なら残すlongのPCMからliveを再構築する。古いsourceのfinalは新しい候補を終了させない。

接続状態は録音状態と別に保持する。BLE切断をfalseへ変換せず、5秒で音声を取り消さない。

## タップ

83のshortと、そのsourceのfinalを確認してからSingle待ち（初期値50ms）へ進む。次もshortならDouble、次のtrueを期限内に観測した場合は後続の分類までSingleを保留する。後続がlongならSingle→録音、shortならDouble。

期限内に次のCが既知で未解析なら、その時点の取得範囲を固定して待つ。後から増えた別のCで待機を延長しない。3回はDouble+Single、4回はDouble+Double。確定したタップはフックに一度だけ渡し、空の認識結果や履歴を作らない。

83の履歴resetや曖昧な対応を、音声長・末尾bit・物理押下時間の推定で補わない。分類できない完成音声は理由をログに残し、独立した全文認識へ渡す。

## 音声・表示・完了

- rawのTLVは一度解析し、デコード済みPCMは共有ブロックで保持する。source IDはdevice・継続世代・開始番号から作る。
- liveは初期値200ms以上の未投入PCMだけを渡す。受信側はASR完了を待たない。全文認識には先頭と末尾の端数を含める。
- 音量は受信PCMから通知し、ASRの結果やlive有効設定を待たない。
- hidden候補や旧世代のpartialは表示しない。Escで閉じた項目は同じ録音の再開・partialでも勝手に再表示しない。
- 受信プロセスがclockも順序付きで送る。IPCが滞留しても、認識側だけのwall-clockで未処理入力を追い越してSingleを発火させない。
- 受信ストアは認識完了または確定タップの破棄ACKまで音声を保持する。容量制限・欠番喪失は明示エラーとし、成功した全文として扱わない。

## 設定とログ

`Settings.reception` の各期限は候補開始時の値を使う。ファイル変更後は既存のreloadで反映する。省略時は表示・再開・タップが各50ms、live投入200ms、状態確認の開始間隔50ms。

`Reception observation / snapshot / action`、`Button history`、`PCM store`、`live PCM / whole PCM` を `--log` に記録する。`button_timing` はS観測の診断であり、物理エッジの復元やジェスチャ分類には使わない。

## 残っている受信スケジューラーの変更

現在は待機中のS周期を設定から読み、音声Cの転送中は約100msごとに完了済みREADの後でSを読む。小さなCを先に回収する分岐と定期Rも残る。これを単一のS/R/C選択器へ置き換え、要求数・期限超過・接続維持時間を記録する作業は継続中。

接続の「未操作1時間 / 通信障害1分で広告待ち」、ASRワーカー異常時の再起動・PCM再送も未完了。受入条件は [BLE受信設計](ble-reception-design.md)、プロトコル根拠は [解析記録](ble-protocol-findings.md) を参照。

## 画面を出さない検証

`cargo test --release --lib --bin pebble-index --bin index-voice --test adapters --test reception_pipeline`

純粋な状態遷移、受信ストア、表示モデル、合成TLVからmock ASRまでのIPCを確認する。Bluetooth実機・マイク・ウィンドウ表示は実行しない。
