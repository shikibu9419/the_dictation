# デスクトップの録音・表示状態

## 責務

- `capture.rs`: BLEの観測値を通知。解放は250msの安定確認後に通知する。音声の転送完了とは独立。
- `gestures.rs`: 短押しの連打判定とフック。未分類の次の押下がある間は単押しを確定しない。
- `continuation.rs`: 音声の連結とEOF。500ms以内の押し直しを同じ録音IDへまとめ、最終チャンクまでPCMを保持する。画面の録音状態を決めない。
- `interaction.rs`: デスクトップの状態を所有し、録音ID付きの `Activity` を出力する。未確定の押下・空音声を画面へ流さない。
- `recognition.rs`: `Activity` を画面へ転送。解放確認済みの録音について音声EOFで全文認識を開始する。切断中止時はPCMを破棄し、認識しない。
- `overlay/model.rs`: 同じ録音IDの表示項目を更新する。再開時も新しい仮項目を作らず、完了済み・閉じた項目を遅延通知で復活させない。
- `overlay/presentation.rs`: 設定と項目フェーズから円形／文字表示を決定する。

## トリガと状態遷移

| 入力 | 状態・効果 |
| --- | --- |
| 押下候補 | Idleのまま。まだ表示しない |
| 150msの長押し＋実音声、または150ms分の音声 | Recording。ID付きで一度だけ開始通知 |
| 短押し確定 | Idleのまま。500msの連打判定後にジェスチャの割当操作 |
| 安定した解放通知 | Dictating。画面はReceivingになり、赤い録音表示を停止 |
| 500ms以内の再押下、同じ録音が未完了 | 同じIDでRecordingへ戻る |
| 遅延した音声チャンク | PCMを追加。解放済みならRecordingへ戻らない |
| 最終チャンクと連結猶予の終了 | 解放確認済みならDictating／Finalizing。押下中ならEOFを保留 |
| 認識完了 | 他に処理中録音がなければIdle。画面はReady、表示設定に従う |
| BLE切断 | Error／Reconnectingで5秒待機し、録音を中止してIdleへ。全文認識しない |
| 再接続・残データ到着 | 中断した録音は読み捨てる。新たな押下を観測するまで新規録音を開始しない |

Dictatingは「残りの音声を受信中」と「認識中」の両方を含む。UIではReceivingとFinalizingで区別する。接続の復旧だけではRecordingへ戻さない。

## 表示の順序

`update_panel` が現在のモデルから表示形式・サイズを計算し、ネイティブのサイズ変更を先にキューへ入れる。非表示ウィンドウではmacOSの描画タイマーが停止するため、ネイティブのレイヤー描画を明示要求する。GPUIのフレームコールバックからネイティブの表示処理をキューへ入れ、描画処理が戻ってから表示する。コールバックは更新番号を照合し、閉じる操作・別の項目・設定変更で古くなった表示要求を捨てる。`render` からウィンドウを表示・リサイズしない。

## 調査根拠と回帰確認

2026-10-08のログでは録音2351の解放を17:22:27.597に観測、17:22:29.024に切断、最終チャンクは17:22:50.578に到着していた。旧実装はEOFまで解放通知を保留していたため、約23秒間Recording表示が残った。

このイベント順序をInteractionのヘッドレステストで再現する。現在の仕様では切断後5秒で中止すること、遅延EOFが認識に流れないこと、同じIDでの短時間再開を確認する。GUIモデルのテストではID付き開始・解放・再開・中止・完了・閉じる後の遅延通知を確認する。実機表示テストは利用者の作業を妨げるため実施しない。

## Updated disconnect policy

A disconnect is not a release. It enters Error/Reconnecting for five seconds,
then emits Cancel and returns to Idle without batch recognition. Recovered
chunks of the interrupted recording are discarded. A fresh press is required
before starting another recording. EOF is held until a debounced release edge
has been observed; EOF alone is not a dictation trigger.

The latest six nonempty recordings inspected were 2311, 2322, 2342, 2351,
2441, and 2453. For 2453 the disconnect at 17:36:11.515 preceded the release
observation at 17:36:29.886. The old disconnect path incorrectly emitted
Activity(false). The new Interrupted/Cancel events are separate from Activity.

With live conversion mode disabled, no phase (including Ready and errors)
may select a Transcript surface unless the user explicitly opened history.
Ready selects Hidden. Legacy live_text=false migrates to live_mode=false.

Button timing logs measure received collecting edges, not physical switch
edges. The five-tap calibration must compare these observations with decoded
sample counts and short/long metadata; missing BLE edges cannot establish
physical tap durations.

## Five-tap calibration (2026-10-08)

The user's latest five single taps are collections 2530 through 2534:
17:37:29.676, 17:37:31.629, 17:37:33.217, 17:37:34.537,
and 17:37:37.027. Every collection contains four PCM samples at 9997 Hz
(0.400 ms of dummy audio) and ends in short metadata. Each generated exactly
one SingleTap event. The cumulative short metadata counts are 1, 2, 3, 4, 5;
these are stored history entries, not five new taps on the last collection.

The activation threshold is reduced from 350 to 150 ms, matching the existing
minimum useful audio duration. A typical first voice chunk of 170-330 ms can
now activate immediately instead of waiting for another chunk. Empty taps
still cannot activate the recording UI. Double-tap grouping remains 500 ms.
This calibrates PCM classification against the five observations; it does not
claim that a physical tap lasted 0.400 ms or determine physical switch timing.
