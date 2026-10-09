# デスクトップの受信・録音状態

## 責務

| 層 | 実装 | 所有するもの |
| --- | --- | --- |
| BLE取得 | `capture.rs / bluetooth.rs / reception/scheduler.rs` | S/R/C要求選択、取得時刻とsequence、同じFIFOへ送るclock |
| 音声ストア | `recordings.rs / pcm.rs` | source ID、不変PCM、連続して取得済みの範囲、欠番とfinal |
| 正規化 | `reception/button_detector.rs` | 83の既出prefix、新規short/long、判定根拠 |
| 状態遷移 | `reception/session_state.rs` | sourceの所属、候補、UI/再開/タップ期限、snapshot |
| 音声投入計画 | `reception/input_effects.rs` | live cursor、共有PCMへの範囲参照、全文ジョブ、世代 |
| 入力アダプター | `adapters/input/index.rs / interaction.rs` | 上記の接続、source解放のACK、割当フック |
| 認識制御 | `recognition.rs / recognition/recovery.rs` | live/batch、録音IDごとの表示許可・世代照合、ASR再起動とPCM再送 |
| 表示 | `overlay/model.rs` | ID付き進捗、編集・非表示・履歴。独自の押下タイマーは持たない |

`gesture_types.rs` はフックに渡す値、`gestures.rs` は動作の割当口。音声やモデルに依存しない。

## 観測から録音へ

Sの `in_collection_state=true` で候補を作る。音声はsourceごとのストアへ保持し、表示待ち（初期値50ms）を過ぎても収集中ならrecording表示を出す。50ms経過はファームウェアのlong確定を意味しない。

falseまたは対応するfinalで終了猶予（初期値50ms）を始める。同じ終了情報で期限を延長しない。猶予後はdictatingへ移り、新しいlive入力を止めるが、音声回収は続ける。所属する全sourceのfinalと欠番なしを確認して全文認識を開始する。

未完了の同じsourceが再び収集中なら同じ録音へ戻る。別のfinal済みsourceとの結合は、猶予内の再開かつlong同士と確認した場合のみ。再開候補がshortなら本文へ混ぜず、必要なら残すlongのPCMからliveを再構築する。古いsourceのfinalは新しい候補を終了させない。

接続状態は録音状態と別に保持する。BLE切断をfalseへ変換せず、5秒で音声を取り消さない。

## タップ

83のshortと、そのsourceのfinalを確認してからSingle待ち（初期値100ms）へ進む。次もshortならDouble、次のtrueを期限内に観測した場合は後続の分類までSingleを保留する。後続がlongならSingle→録音、shortならDouble。

期限内に次のCが既知で未解析なら、その時点の取得範囲を固定して待つ。Sの低8bit件数の変化だけが先に分かった場合は、Rの応答で範囲を確定してから、その範囲のCを待つ。8bitから16bitの件数を推測しない。後から増えた別のCで待機を延長しない。3回はDouble+Single、4回はDouble+Double。確定したタップはフックに一度だけ渡し、空の認識結果や履歴を作らない。

ダブルタップの割り当てが `none`（操作なし）の場合、入力アダプターが実効 `tap_sequence_grace_ms` を0にする。状態機械はshort+finalと欠番なしを確認した同じ遷移でSingleを発火し、pending・後続C待ち・Doubleへの結合を作らない。連続するshortはそれぞれSingleになる。保存された待ち時間は変更せず、割り当てを戻せば再び適用する。検出器と状態機械はUIの割り当てを参照しない。

独立した1個の完成Cに83が1項目だけある場合は、そのsourceの分類として扱う。別sourceに同じshortが続いても重複ではない。C/sourceの再送は受信ストアで排除する。複数項目の履歴resetや曖昧な対応を、音声長・末尾bit・物理押下時間の推定で補わない。分類できない完成音声は理由をログに残し、独立した全文認識へ渡す。

## 音声・表示・完了

- rawのTLVは一度解析し、デコード済みPCMは共有ブロックで保持する。source IDはdevice・継続世代・開始番号から作る。
- liveは初期値200ms以上の未投入PCMだけを渡す。受信側はASR完了を待たない。全文認識には先頭と末尾の端数を含める。
- 音量は受信PCMの末尾40msから通知し、ASRの結果やlive有効設定を待たない。転送待ち中は最新Cを先読みし、現在の録音に帰属する音量だけ通知する。先読みはsource・final・ボタン履歴・取得カーソルを進めない。
- 音量フィードバックのライフサイクルは表示モデルの `Phase::Recording` が所有する。状態入力 `index_panel_audio_state` と音量入力 `index_panel_audio_level` を分け、チャンクから録音状態を開始・終了しない。録音IDで配送先を照合し、対象外のチャンクは無視する。新しい録音は前の音量を引き継がない。
- recording中、正規化音量0.35以上なら伸縮を継続し、0.25以下なら静止へ移る。各伸縮で時間（0.20〜0.45秒）と到達値（山78〜100%、谷22〜48%）をランダムに選ぶ。前の終点からsmoothstepで補間するため、境界で位置・速度が飛ばない。音量は60ms、動きの出入りは120msで補間する。これは表示演出でありVADや実音声波形の再生には使わない。チャンク待ちを無音とみなさず、録音終了・切断・非表示で状態を抜けたら即座にリセットする。静かな音量へ移り終えたら描画タイマーを止める。macOSの「視差効果を減らす」有効時は周期運動を使わない。
- 先読みCのrawは受信側で保持し、順次回収時に再要求せず渡す。デコード済みPCMも最大512件・8M samplesまでキャッシュし、通常の音声ストアへ移す。各先読みの間に少なくとも1件は通常のCを進め、待機中や全文回収のみの時は先読みしない。
- hidden候補や旧世代のpartialは表示しない。Escで閉じた項目は同じ録音の再開・partialでも勝手に再表示しない。
- 受信プロセスがclockも順序付きで送る。IPCが滞留しても、認識側だけのwall-clockで未処理入力を追い越してSingleを発火させない。
- 受信ストアは認識完了または確定タップの破棄ACKまで音声を保持する。容量制限・欠番喪失は明示エラーとし、成功した全文として扱わない。

## シングルタップによるキャンセル

`SessionState::tap` がshort確定時に、そのcollectionより前の未完了録音を選ぶ。対象があれば、通常の100ms待機・watermark待機・Double結合より先にキャンセルとして消費する。対象session IDをgestureに保持し、そのsessionをキャンセル要求済みにする。キャンセルの押下をpendingや次の操作のprefixに残さず、次の押下とは結合しない。対象のない通常操作だけがシングル／ダブル判定を使う。

`recognition/cancellation.rs` は指定された録音IDを無効化し、`cancelled` と処理済みのgestureを送る。受信後に始まった別の録音へキャンセル先を変更しない。判定と最終結果の送信を同じロックで直列化し、キャンセル後のlive/batch結果を出力しない。結果の確定が先に完了した場合も、キャンセル操作をペースト等の通常割り当てへ転用しない。

処理中の認識はwatch通知で割り込み、エンジンのcancelを送る。PCMのJSON書き込みは途中で切断せず、応答待ちを中断する。2秒でcancel応答がない場合はそのワーカーを復旧し、キャンセル済みのjournalは再送しない。待ち行列の対象ジョブは認識せず破棄する。回収中の音声はfinal・欠番なしを確認してから完了ACKを返し、ストアとcheckpointを通常どおり解放・更新する。これにより次回起動でキャンセルした録音を再処理しない。

キャンセルは押下より前の最新1件に限定し、他の録音や完了済み履歴は保持する。`Single tap cancellation` に対象録音ID、受付結果、gestureのcollection範囲、ダブルタップ待機を省略したことを記録する。認識ワーカーにはcancel送信・応答を記録する。

## 設定とログ

`Settings.reception` の各期限は候補開始時の値を使う。ファイル変更後は既存のreloadで反映する。省略時は表示・再開が各50ms、タップ連結100ms、live投入200ms、状態確認の開始間隔50ms。

`Reception observation / snapshot / action`、`Button history`、`PCM store`、`live PCM / whole PCM` を `--log` に記録する。`button_timing` はS観測の診断であり、物理エッジの復元やジェスチャ分類には使わない。

## 受信スケジューラー

`reception/scheduler.rs` が一度に一つのREADを選ぶ。Sは設定された開始周期、Cは既知の未取得番号順、Rは件数変化・回収完了・1秒の周回確認をまとめる。Cの長さによる分岐と固定sleepは削除した。状態期限を過ぎたら進行中のREAD完了後にSを行う。S自体が周期を超える場合はR/Cも一件進め、遅いRが続く場合も既知のCを一件進める。遅れた周期を取り戻す連続要求は作らない。

音声Cは受信直後に配送する。`BLE schedule` は要求と期限超過、`BLE read totals` はS/R/C別の累積要求数・失敗数・通信時間・応答バイト数を60秒ごとと切断時に残す。接続継続時間も切断時に記録する。電池残量を読む命令はこの実装では未確認のため、消費率を推定した数値は表示しない。実機の電池推移の測定は未実施。

## 接続維持と広告待ち

`reception/connection.rs` が接続の寿命を決める。収集中でも回収待ちでもない状態で1時間操作がなければ切断して広告待ちに入る。Sの収集中フラグ、件数の変化、Cの受信が操作時刻を更新する。通常のS成功だけでは未操作タイマーを延ばさない。

接続・通信の失敗時は250ms〜5秒の間隔で保存UUIDへ再接続する。連続1分の障害後は広告待ちへ移る。接続できただけでは障害期間を消さず、S/Rで回収待ちなしを確認するか、Cの転送が進んだ時点を復旧とする。ペアリング不整合・不正データのエラーは自動再試行を停止する。

広告待ちではCoreBluetoothの探索を維持し、同じ広告の反復では接続しない。件数・収集中・動き・機器fingerprintの変化、2秒以上途絶えた広告の再出現、Macのwake/session復帰、Bluetooth復帰で先行接続を試す。障害後の広告待ちに限り、同じ広告でも30秒間隔の復旧確認を許可する。未操作による広告待ちは、この定期確認を行わない。これは接続を再開するヒントであり、広告からshort/longや物理エッジは作らない。

`BLE failure` に失敗段階・OSエラーのdomain/code・再開cursor、`BLE reconnect hint` に再開理由、`BLE communication recovered` に障害からの復旧時間を残す。再接続時も受信cursorと音声ストアを維持し、起動時のflushを繰り返さない。実機の復帰時間・電池消費の測定は未実施。

## ASR障害からの復旧

ASRのEOF・エラー・応答タイムアウトは `recognition/recovery.rs` で扱う。故障したliveまたはbatchの子プロセスだけを閉じて再起動する。入力のデコード・受信ストア・状態機械・もう一方のASRは継続する。再送用のjournalはPCMの共有参照を持ち、送信前に更新するため、受領ACK前の切断でも投入途中の音声を失わない。

新しいモデルは推論状態を失っているため、現在の論理録音の先頭から再送する。受領済みPCMも含めてフィルター・窓・prefixを再構築し、未処理ジョブはその後に続ける。新しいBLE読み出しは要求しない。journalの上限は64Miサンプルで、超過時に黙って切り詰めない。再起動中に終了・世代変更されたliveは破棄し、元の全文ジョブは残す。

Qwenの再送中は、消費cursorが以前の表示位置に追いつくまで古いpartialを表示しない。消費cursorを返さない従来の外部ASRでは、この位置による表示抑制は行わない。finalを受け取って表示した後の制御エラーでは、その録音を再認識せず一度だけ完了ACKを渡す。

`RestartableControl` がlive/batchの実行許可を新しいモデルへ引き継ぐ。旧プロセスが終了するまでは停止済みと扱わない。再起動中の許可変更と旧プロセスの遅いACKを世代で照合し、停止中のbatchが復旧直後に勝手に推論を始めることを防ぐ。許可の送信失敗も、当該ワーカーの復旧ループを起こす。

初回は直ちに再起動し、連続失敗時は500msから最大30秒まで待ちを延ばす。モデルのready待ちは240秒。起動に失敗した子も閉じてから再試行する。設定・不正PCM・保存先のエラーはASR再起動で握りつぶさない。

合成TLVとmock ASRのIPCテストは、ACK前後の終了、finish中の終了、再起動中のrelease、再起動失敗後の再試行、連続録音を含む。実Qwenの追加テストでは、自分で起動したliveと停止中のbatchを各一度終了させ、日本語4.14秒の全文をそれぞれ一度だけ確定した。

IPC上限の確認と受入条件全体の監査は継続中。受入条件は [BLE受信設計](ble-reception-design.md)、プロトコル根拠は [解析記録](ble-protocol-findings.md) を参照。

## 画面を出さない検証

`cargo test --release --lib --bin pebble-index --bin index-voice --test adapters --test reception_pipeline`

純粋な状態遷移、受信ストア、表示モデル、合成TLVからmock ASRまでのIPCを確認する。Bluetooth実機・マイク・ウィンドウ表示は実行しない。

広告フィルターは次の専用mainで検証できる。`Bluetooth`・`CBCentralManager`・`NSWorkspace`を生成せず、合成広告だけを扱う。

```sh
xcrun swiftc -O -parse-as-library -D TEST_BLUETOOTH_POLICY \
  native/Bluetooth.swift tests/native/BluetoothPolicyTests.swift \
  -o /tmp/index-bluetooth-policy-tests
/tmp/index-bluetooth-policy-tests
```
