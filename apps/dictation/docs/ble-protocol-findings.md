# Index 01のshort/long・音声転送の解析根拠

2026-10-09。実ログ・現行コード・固定依存の配布コードの静的解析。提案は [BLE受信とジェスチャ判定の再設計案](ble-reception-design.md) を参照。

## 2026-10-09 09:12 の短押し取りこぼしと音量遅延

- C2804のlong後、C2805 / 2806 / 2807は別々の完成した非multipart C。83は毎回 `0000000001000000`（shortが1項目）、84は4317 / 4318 / 4319へ進んでいた。受信時刻は09:12:20.945 / 25.535 / 31.355。同じ83値をデバイス共通の重複とする旧判定が3回とも操作を捨てていた。
- C2808は83がshort 2項目に増え、旧判定でも差分shortとして発火。09:12:34.495に貼り付け成功の応答がある。別sourceの完成Cに1項目だけある83はそのsourceの分類として採用し、同じCの再送はsource/collectionの台帳で排除する。
- 09:11:13.024→13.265、15.034→15.304のshort受信間隔は241 / 270ms。ただし次の件数変化は13.085 / 15.094、つまり61 / 60ms後に観測済み。一度300msへ広げたが、単押しを余分に待たせるため100msへ短縮した。期限内の件数変化で後続Cの解析を待つ仕組みがあるので、この2例の転送完了を100ms以内に収める必要はない。物理押下時間の測定ではない。
- 長押しsource2719はS=trueが09:11:27.964、falseが09:11:51.935。最終C2804の到着は09:12:16.445。C2720は約200msの音声に対して転送482ms、C2721は約185msに対して480ms。音量計算やASRより前に、古いCを順に取得する段階で待ちが積み上がっている。
- 対応：最新Cを表示用に先読みして保持し、本文とジェスチャは順序を守って処理する。Cの生成・リンク速度の制約そのものが解消したという意味ではない。S/Rの要求頻度は増やさず、遅いRの直後にSを重ねて音声を待たせるスケジュールも除去した。

## 1. 確認できたデータの意味

### 2026-10-09 10:27〜10:28のBLE取得後の遅延

| C | 受信→Single確定 | Single確定→貼り付け送信応答 | 受信→応答 |
| --- | ---: | ---: | ---: |
| 2816 | 102ms | 215ms | 317ms |
| 2817 | 101ms | 172ms | 273ms |
| 2822 | 101ms | 176ms | 277ms |
| 2828 | 100ms | 201ms | 301ms |

各Cの `Input delivery queue_ms` は0ms。貼り付けヘルパーは前面アプリにも25msのポーリング待ちと100msの固定待ちを課し、その後AXメニューを探索してからキーボード送信へフォールバックしていた。単押しで入力先が既に前面なら、この待ちとメニュー探索を省いて送信するよう修正した。フォーカス復元が必要な場合とEnter操作の処理は維持する。検出後→GUI→バックエンド→ヘルパーの計測を追加した。この時点でタップ確定待ちを70msにした。その後の指定で100msに戻し、ダブルタップ「操作なし」の場合は確定済みshortを待機なしでSingleとして渡す。

BLE側の待ちも独立して存在する。C2816 / 2817の86〜87byte取得は510 / 541msで、最初の通知まで509 / 539ms。一方C2822は約60ms。このログだけでは無線・リングの処理・macOSのどこが原因か断定できない。READの応答待ちを、ASRやタップ判定の待ち時間に含めて説明しない。

| 情報 | 分かること | 分からないこと |
| --- | --- | --- |
| R: 0x40030005、4 bytes | 保存済みcollectionのstart/end（16bit、end exclusive） | 物理押下の時刻・持続時間 |
| C: 0x40020000 OR index | 指定collectionの音声・メタデータ | 受信時点でボタンが押されているか |
| S: 0x4003000e、10 bytes | 件数下位8bit、収集中フラグ等の現在観測 | 精密なDown/Up、確定済みshort/long |
| record82 | 開始番号、multipart、final | 1列が必ず1回の物理押下と一致する保証 |
| record83 | pattern:u32+count:u32。bit 0=short、1=long | 列の末尾が当該Cの操作か、精密な押下間隔、count>32の意味 |
| record80/81 | PCM/DDRice圧縮音声、rate | PCMがあるだけでは発話・長押しとは判定できない |

READは13-byte Control要求、12-byte Control応答とData通知。応答に要求IDがないため同時READは1件。pushでボタンエッジが届く仕様や、変更まで保留するREADは確認できていない。

### shortでも音声が付く

10/8の「shortだけの列・非multipart・final」の57観測には、4 samplesが49件、5が6件、6が1件、80が1件あった。rate=9997 Hzなので4 samplesは約0.4ms。

また、列の末尾がshortの非multipartには2723 samples（約272ms）の例もある。ただし過去のlongも含む列なので、これを「そのshortが272ms録音した」とは解釈できない。**ダミー音声は常に4 samples、short音声は必ず150ms未満、とも断定できない。**

### longより前に音声が来る

10/8の録音データ列2605の実例:

| 受信時刻 | 観測 |
| --- | --- |
| 19:30:48.639 | Sの収集中=trueを観測 |
| 19:30:50.232 | 最初の音声8655 samples、multipart=true、final=false、押下列=[] |
| その後 | 音声Cが継続、押下列は空のまま |
| 19:31:15.669 | Sの収集中=falseを観測。アプリがHoldReleasedを生成 |
| 19:31:23.711 | final=trueを受信、押下列=[long] |

これらはMac受信時刻であり、物理押下/解放時刻ではない。**longを待ってライブ認識を始めると、この例では最後まで始まらない。** longの値が繰り返されても、履歴が再掲されている可能性があり、押しっぱなしを意味しない。

別の列2579では開始・途中が[short]、finalが[short,long]。列2441では音声の途中に[long,short]、[long,short,short,short]等へ増えている。record82とrecord83を1対1の物理操作として扱う設計は成立しない。

### 録音とチャンク生成の周期

公式仕様は「ボタンを押している間だけ録音する」。物理的に押していない間の常時録音はしない。[公式FAQ](https://repebble.com/index)、[開発者説明](https://developer.repebble.com/index-01/)

一方、record83が空でも録音中の音声は届く。ボタン解放後も、保存済み音声の回収は続く。したがって**押下列なし・現在S=false・新しい音声の受信なし、は相互に同じ意味ではない。** 音声以外の内部記録まで一切ないとは確認していない。

10/8の未完了multipart 299観測:

- collectionサイズ: 4073〜4096 bytes、中央値4094 bytes。
- 含まれる音声長: 約172〜1027ms、中央値293ms。
- 最初のCを観測した11列の先頭音声長: 約841〜998ms。

これは音声長であり、生成間隔や受信間隔の実測値ではない。ほぼ4KiBの圧縮データ量を目安に分割していると推測できるが、ファームウェアのflush条件は未確認。固定20ms/50msごとにCが作られるとは言えない。

### スマホでの扱い

#### 音声取得を始める条件と完了条件

配布SDKの `shouldTransferCollections`（iOS逆アセンブル41542行付近）と同版Androidの `shouldTransferSwings` を照合した。通常動作・転送許可ありの場合、次のいずれかで取得を開始する。

1. 広告/状態の `inCollectionState` がtrue。
2. 前回の取得末尾が未登録。
3. 前回取得末尾を広告用の件数へ切り詰めた値と、今回の `collectionCount` が異なる。

この取得開始の条件式にはshort/longは入っていない。収集中フラグによって回収を先に始め、Rで範囲を調べ、Cを取得する。`RingSync.kt` は `TransferTypeDetermined.isAudio` と新しい `collectionStartIndex` から音声の転送項目を作る（399行付近）。開始専用のジェスチャイベントを待つ経路ではない。

multipartはSDKの `processMultiPartAudio` が開始番号ごとに `addPart` し、音声timelineの `isFinalPart` がtrueなら `emitCompleteTransfer` する（`mapping.txt` 3280〜3350行付近）。これはrecord82のfinal。アプリ上では通常の長押し録音が終了した合図として使える。受信時刻そのものをボタン解放時刻にはしない。

モバイルのこの経路は、`TransferComplete` 後に全音声をresample・保存し、`queueAudioProcessing` に渡す（`RingSync.kt` 559、655、726行付近）。転送開始時には認識エンジンをearlyInitしているが、途中の各CをライブASRへ投入する処理ではない。ここには完成音声が1秒未満ならDiscardedにする条件もある。これは認識対象を選ぶ条件で、short/longを作る閾値ではない。

モバイルのTransferTypeDeterminedは音声timelineの有無とbuttonSequenceを別に渡す。ButtonSequenceDebouncerは列のprefix拡張、回収範囲の末尾、収集中状態等で列をまとめる。short/longの末尾を現在のボタン状態として使っていない。

モバイルはTripleも含む列全体をまとめる。待ち窓は700ms。収集中・回収範囲の末尾でない場合は待ちを更新し、それ以外はreleaseTimestampからの経過を差し引く経路もある。常に受信後700ms待つわけではない。このアプリのSingle/Double即時hookへ、その待ちをそのまま持ち込まない。モバイルのreleaseTimestampにはcollection時刻+音声長から作る経路があり、ファームウェアが精密なUp時刻を送っている根拠にもならない。

## 2. 途切れについてログで判明したこと

**アプリ側で、final以外を理由に停止・取消する経路がある。**

10/8、録音データ列2585:

1. 19:30:32.161、BLE切断。live停止と5秒の取消タイマー開始。
2. 19:30:33.670、再接続成功。34.392から同じ開始番号の音声回収を再開。
3. 19:30:37.164、古いタイマーでRecording cancelled、全文認識なし。GUIも消える。
4. 19:30:41.173、同じ列のfinal=trueが到着。

この取消はリングのfinalによるものではない。**復旧しても取消タイマー/エラー状態を解除しないアプリの不具合**が記録されている。

10/7の列2215ではSがfalse→trueへ変わる間も同じ開始番号の非final音声が届き、finalは後から届いている。Sの変化だけで音声列を分割してはいけない。

ただし、ユーザーが物理的に押し続けた時刻の記録はない。リングが本当に押下中にfinalを生成したか、接点・ファームウェアが異常だったかは、このログだけでは断定できない。BLE切断自体の原因も別に調べる必要がある。


## 3. 配布コードで確認した範囲

調査対象は、このリポジトリが固定しているHaversine 134bcb9のiOS配布バイナリと同版の共通Kotlinコード。これはリングのファームウェア本体ではない。

- iOSの `PPCollection_buttonPressSequenceString`（逆アセンブル188521行付近）はrecord83のpattern/countを読み、下位bitから0をshort、1をlongへ展開する。ここに押下時間を測定して分類する処理はない。
- patternの幅は32bit。現行Rustはcount<=32だけを受理するが、iOSの文字列変換関数には同じ上限チェックを確認できない。リング側で32要素に飽和・リセットする保証までは得られていない。
- `PPCollection_createAudioTimeline`（188382行付近）はrecord82と80/81を読む。83の末尾がshortかlongかによって音声を捨てる分岐はない。
- Kotlinの `TransferTypeDetermined` は音声timeline、buttonSequence、開始番号、収集中状態を別々に保持する。finalのreleaseTimestampをcollection時刻＋音声長から作る経路がある。
- `ButtonSequenceDebouncer.supersedes` は「新しい列が古い列より長く、古い列をprefixに持つ」を確認する。双方のreleaseTimestampがある場合は差が3秒以内かも確認し、片方がなければ時刻条件を省く。これはSDKの結合方針であり、リング側のshort/long閾値ではない。
- 同Debouncerの待ち窓は700ms、古いイベントを扱わない基準は30秒。タイマー世代を照合し、古いタイマーは無効化する。
- モバイルの `RingGestureRouting.kt` は列全体をClick=[short]、DoubleClick=[short,short]、TripleClick=[short,short,short]、Hold=[long]、ClickHold=[short,long]へ対応させる。longの再掲はHeldの定期通知ではない。

### 接続・取得方式

- Telesto Control: `c0ef558a-2058-fabf-a140-8d5acde50b39`、Data: `daad3d52-237c-90a7-b54b-8854a134d801`。
- READはlittle-endianのopcode=3:u8、address:u32、offset:u32、length:u32。Cのlength=0は全体取得として使用している。Data通知がControl応答より先に来ることも想定した受信器が必要。
- 現行コードから使い方を裏付けられる取得はR/S/C。モバイルは広告の件数・収集中フラグ等から転送要否を決め、R→C群→Sの転送操作を行う。キューが空になると切断する経路がある。
- `RingSync.kt` の3秒はスキャン終了後の再開待ち。R/Sを3秒ごとに読む指定ではない。固定20ms/50ms等のポーリング周期は配布コードから確認できていない。
- `HaversineReadLastAudioSamplesOperation` は保存範囲・末尾C・multipartを読む処理で、マイクの最新サンプルがプッシュされるAPIと確認できたわけではない。
- `SensorStreamOperation` はセンサー設定・FIFO等を扱う。SystemInputの7/8などを見つけたことだけを根拠に、Indexの音声プッシュとして使用しない。
- offset/lengthが存在することだけでは、Cの一部だけを安全に読み出せる保証にならない。メタデータ専用READ、長時間保留するREAD、押下エッジ通知、接続中の広告受信保証はいずれも未確認。

### longはfinalと完全に同時でもない

列2342のC2349は17:21:52.587に `multipart=true, final=false, buttons=[long]`、C2350は52.647に `final=true` で到着した。S=falseは50.516に観測されていた。従って「longは必ずfinalだけに付く」も誤り。確定操作の履歴と音声列の完了情報は別に読む。

### Doubleの実際の早期Single確定

10/8:

| C | 受信 | 後続Cの存在をRで認識 | 誤ったSingle | 次のCの受信 |
| --- | --- | --- | --- | --- |
| 2673→2674 | 19:31:42.282 | 42.370 | 42.385 | 42.431 |
| 2675→2676 | 19:31:44.173 | 44.231 | 44.275 | 44.353 |

「次のCが存在する」と分かっているのに、タイマーがSingleを確定している。100msを大きくするだけでは、通信がさらに遅い場合に再発する。

またC2672〜2676ではshort列が1→2→3→4→5と伸び、操作群間には約2秒の受信差もある。列の要素数が2以上なら無条件にDouble、という読み方も成立しない。消費済みprefixと候補の区切りが必要。

## 4. 現行コードに対する監査

| コード | 確認した動作 |
| --- | --- |
| `src/capture.rs` の `Received::state` | 250ms確認より先にbutton_stateを送るため、下流の押下判定はこの確認を受けない |
| `button_detector.rs` | S由来の疑似Downから50msでHoldStarted。short受信後100msのSingle確定が未取得Cを知らない |
| `index.rs` | ボタン列の末尾だけを、音声のfinalが出た時に処理する |
| `input_effects.rs` | captureなし/Error中のPCMを捨て、sourceをその時のpress-IDへ紐付ける |
| `interaction.rs / session_state.rs` | 切断後5秒でCancel。復旧成功で期限・Errorを解除する入力がない |
| `recordings.rs` | TLVを二度解析。非multipartかつ150ms未満ならボタン列を問わずPCMを空にする |
| `recognition.rs` | 全文Vecとlive/batchのジョブを保持。acceptedは消費完了ではない。音量通知はlive無効の場合に限定 |
| `native/qwen/adapter.py` | `chunk_size_sec=1.0`。パイプ受信ACKは推論進行より先に返る |

Rust側の `pcm.chunks(rate / 2)` は最大0.5秒へ分割する処理で、0.5秒貯まるまで待つ実装ではない。この分割だけを短くしてもQwen内部の1秒待ちはなくならない。

## 5. 再現可能な参照先

- 既存ログ: `~/Library/Logs/Index Voice/debug.log`。2585の切断は344510行付近、再接続344521、取消344598、final344694。2605の先頭Cは344950、finalは345695付近。ログローテーション後は日時・C番号で検索する。
- iOS逆アセンブル: `desktop/ios-analysis/artifacts/disassembly.txt`。`PPCollection_*`、`HaversineReadLastAudioSamplesOperation`、`SensorStreamOperation`、`shouldTransferCollections`。
- 共通Kotlinの解析出力: `/tmp/index-mobile-design/classes.jar`、`mapping.txt`、`debouncer.txt`、`transfer.txt`、`scan.txt`。一時ファイルなので、恒久的な根拠として関数名と判定規則を本書にも残した。
- モバイルソース: [RingGestureRouting.kt](../../../experimental/src/commonMain/kotlin/coredevices/ring/service/button/RingGestureRouting.kt) と同ツリーの `RingSync.kt`。
- 10/7〜10/8の集計は受信観測数。物理操作数・物理押下時間・電池消費を測定したものではない。

リング側のshort/long閾値、83のリセット/飽和条件、Cの生成/flush条件は未確定。SDKに見つからないことを「ファームウェアにも機能が存在しない」という証明にはしない。
