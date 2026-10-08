# Pebble Index 01 — Rust版

> **非公式・自己責任での利用について**  
> このソフトウェアは、Pebble Index 01をMacへ直接接続して使用する非公式の実装です。メーカーが提供する本来の利用方法とは異なります。使用・改変・開発は、ご自身の責任で行ってください。動作やデータの保全を保証するものではありません。

Index 01の音声をMacへBLE転送し、標準で **Apple SpeechAnalyzer / SpeechTranscriber** を使って文字起こしします。通常起動・Webhook・ファイル文字起こしは共通の認識アダプターを使います。SpeechAnalyzerとOn Device（Qwen3-ASR MLX）はどちらもPython・uvなしで実行します。旧`desktop/python`には依存しません。

この独立した `desktop/rust` リポジトリの独自実装は **Apache License 2.0** で提供します。[LICENSE](LICENSE) と [NOTICE](NOTICE) を参照してください。依存ライブラリ・モデル・Apple SDK等にはそれぞれのライセンス・利用条件が適用されます。親の `mobileapp` や他のリポジトリのライセンスを変更するものではありません。

## 必要なもの

- macOS 26以降（SpeechAnalyzerを使う場合はSpeechTranscriber対応Mac）
- Xcode 26以降（初回起動のセットアップを済ませ、Command Line Toolsに選択）
- Rust 1.92以降とCargo
- CMake（MLX等のネイティブ部分のビルド用。未導入なら `brew install cmake`）
- Pebble Index 01、またはMacに接続されたマイク

初回はCargo依存関係とAppleの音声モデルの取得にインターネット接続が必要です。Apple APIを呼ぶSwiftヘルパーは初回実行時にビルドし、以降はキャッシュを使います。SwiftソースはRustバイナリへ埋め込まれるため、実行時にこのリポジトリのソースを探す必要はありません。

On DeviceはApple Siliconを使用します。配布用appにはQwenNativeとMetalリソースを同梱し、設定画面の「モデルをダウンロード」でモデルを用意します。ソースからビルドする場合も、ルートのCargoでCLI・GUI・QwenNativeをまとめて生成します。認識中に音声をサーバーへ送信することはありません。

## ビルド・起動

リポジトリのルートから実行します。

```sh
cd desktop/rust
git submodule update --init -- vendor/mlx-c
cargo build --release --locked --bins
./target/release/pebble-index pair
./target/release/pebble-index --log debug.log
```

Cargoから直接起動する場合:

```sh
cargo run --release -- --log debug.log
```

初回は`pair`を実行します。UUIDはRust版専用の`~/.config/pebble-index-rust/device.json`へ保存します。他のアプリの設定・キャッシュ・ロックは読みません。Rust版の二重起動時は既存PIDを表示してBLE接続前に終了します。

音声認識の準備・Bluetooth待機の表示が出たら、リングを長押しして話し、離してください。接続のために一度押して離す操作は不要です。終了はCtrl+Cです。

## 初めてペアリングする場合

スマホのBluetoothをオフにして、Macの近くにリングを置きます。

```sh
./target/release/pebble-index pair
```

探索中に見つからない場合は、リングのボタンを押して起こしてください。すでに広告を出しているリングは、ボタンを押さなくても検出されます。macOSのBluetoothアクセス許可が表示されたら承認してください。最初のPebble Indexの広告を受信した時点で探索を停止し、UUIDを保存します。30秒は未検出時の待機上限です。`pair`は接続・暗号化通信を行いません。

`pair`は繰り返し実行できます。保存済みUUIDがあればBluetoothを起動せず、そのまま終了します。同じUUIDの設定は書き換えません。`pair UUID`は探索せず指定UUIDを保存します。OS側だけ登録済みの場合も、探索またはUUID指定でRust版に登録できます。

通常起動時に保存UUIDへ接続し、暗号化通信を確認します。OSに正常なペアリングがあれば再利用し、必要な場合はペアリング承認が表示されます。表示されたら承認してください。UUIDの保存だけでは接続成功を保証しません。

複数のリングがある場合は、指定時間探索して一覧を返す `scan` でUUIDを確認し、対象を指定します:

```sh
./target/release/pebble-index scan
./target/release/pebble-index pair UUID
```

暗号化拒否の場合はスマホ側の登録が残っていないか確認し、必要ならスマホで登録解除・リングのリセットを行ってからペアリングします。

`Peer removed pairing information` は、リングのリセットなどでMacとリングのペアリング情報が食い違った場合に接続時に出ます。Macの「システム設定 → Bluetooth」で対象のPebble IndexをControlクリックし、「登録を解除」を選んでから、通常起動し直してください。UUIDの再登録は不要です。Rust版の `device.json` を削除してもmacOSのペアリング情報は消えません。このエラーでは自動再接続を停止します。

## フローティングGUI（GPUI / macOS）

```sh
cd desktop/rust
# GPUIの初回ビルドでMetal Toolchainがないと言われた場合
xcodebuild -downloadComponent MetalToolchain
git submodule update --init -- vendor/mlx-c
cargo build --release --locked --bins
./target/release/pebble-index pair
./target/release/pebble-index gui --log debug.log
```

アプリとして起動する場合:

```sh
sh build-app.sh
open "target/Index Voice.app"
```

メニューバーの波形アイコンが待機中の目印です。既存のCLI受信は停止してから起動してください。同時起動時はBLEの排他ロックによりエラーを表示します。

### リロード

メニューバーの波形アイコン → **リロード** で、BLE／マイク入力と音声認識を停止し、保存済み設定で起動し直します。エラー後も通常の待機中も使えます。メニューが「準備中」から「リング待機中」（マイク入力では「右Optionで録音」）に変われば完了です。録音中の音声と未貼り付けの表示結果はリセットされます。

### 入力と認識モデルの切り替え

ライブ認識と最終認識は別々のモデルを選べます。既存の設定では従来のモデルを両方に引き継ぎます。

- **録音中の文字起こし → 非表示**：ライブ認識を停止し、正円のウィンドウに録音中は赤いマイク、受信・認識中はスピナーだけを表示します。
- ライブ非表示の録音中は、受信PCMの音量に合わせて枠のネオンの広がりが変化します。無音では縮み、受信が止まった場合も減衰します。認識結果や音声判定には影響しません。BLE受信の遅延は残ります。
- **認識完了後の結果 → 表示せず閉じる**：全文認識が完了するとパネルを閉じます。表示する場合は従来どおり編集できます。
- 表示設定によらず、完了時に全文をバックエンド経由でクリップボードへコピーします。空の結果ではクリップボードを変更しません。別アプリへの貼り付けはEnterを押したときに行います。
- 非表示の結果も空でなければ履歴から開けます。エラーは非表示設定でも表示します。

モデル選定（2026-10-08）：日本語精度と既存のMLX環境を優先し、バッチ用のOn DeviceはQwen3-ASR 1.7B 8bitを採用しています。[MLX実装者の日本語10件の比較](https://github.com/moona3k/mlx-qwen3-asr/blob/main/docs/BENCHMARKS.md)ではfp16の0.6BがCER 9.3%・0.51秒、1.7Bが3.6%・1.08秒でした。少数例かつ別マシンの測定で、このMacの速度や8bitの精度を保証する値ではありません。[Cohere Transcribe](https://huggingface.co/CohereLabs/cohere-transcribe-03-2026)も日本語・MLX対応ですが、英語ランキングの首位を日本語・Mac上での最速とは扱わず、今回の既定モデルにはしていません。

メニューバーの **設定…** を開き、入力と音声認識を別々に選んで **保存して切り替える** を押します。

- **Pebble Index**：保存したリングUUIDへ接続します。
- **PCマイク · 右Option**：右Optionを押している間の音声を取り込み、離すとライブ認識を止めて全文を再認識します。左Optionでは録音しません。macOSの「マイク」と「入力監視」の許可が必要です。
- **SpeechAnalyzer**：Appleのオンデバイス音声認識です。初期設定はこちらです。
- **On Device**：Qwen3-ASR 1.7B（8bit）をMLXで動かします。日本語対応、Mac内で認識します。初回は **モデルをダウンロード** で約2.2 GBのモデルを取得・検証します。ライブと最終認識それぞれでSpeechAnalyzer / Qwen3-ASRを選べます。

録音・認識の途中や未処理の結果がある間は切り替えません。結果を貼り付けるか閉じてから保存してください。切り替え時は入力・認識のプロセスを停止し、新しい設定で起動します。

CLIでも同じ設定を使えます。設定ファイルは `~/.config/pebble-index-rust/settings.json` です。

```sh
# ソースからCLIを使う場合のみ。build-app.shでは自動実行します
git submodule update --init -- vendor/mlx-c
cargo build --release --locked --bins
cargo run --release -- setup-qwen
cargo run --release -- settings --input microphone --speech on-device
cargo run --release -- gui
# PCマイクをCLIから使う
cargo run --release -- microphone
# 初期設定に戻す
cargo run --release -- settings --input index --speech apple
```

On Deviceは1秒ごとに途中結果を更新し、最大30秒の音声窓と修正可能なテキスト末尾を使います。計算済みのエンコーダー出力・デコーダーKVを再利用し、窓の切り替えは可能なら無音位置で行います。1秒は更新用の音声量であり、BLE・計算・表示を含む総遅延ではありません。録音終了後は別プロセスで全音声をオフライン認識し直します。長い音声は最大30秒の区間へ分割して全区間を結合し、トークン上限による途中打ち切りは成功として表示しません。

On Deviceの全文認識中に次のlive入力が来た場合は、計算の区切りで全文認識を停止してliveを優先します。停止確認には実行中の計算を終える時間が必要です。live終了後は同じ全文認識を再開します。停止中もBLE受信と音声保持は続け、認識ワーカーへの先送りは最大2窓分に制限します。

- リングを長押しすると、現在のSpaceの画面下中央に小さなパネルを表示し、文字起こしを更新します。通常ウィンドウ・最大化・別アプリのフルスクリーン上でも表示するmacOSの非アクティブ化パネルを使います。
- ボタンを離すとライブ表示を止め、残りの音声の受信、録音全体の認識処理を順に待ちます。物理ボタンの変化から表示までにはBLE広告・状態取得の遅延があります。
- 録音中は左に赤い点、処理中は同じ位置にスピナーを表示します。最終結果はカーソルを末尾に置いて編集でき（Shift+Enterで改行）、**Enter** で貼り付けます。Enterマークと右側の操作欄は表示しません。途中結果はEnterで貼り付けません。
- **Esc** またはパネルの外側をクリックすると閉じます。本文は22.5px、行高30pxの上揃えで、行数に合わせて下端を固定したまま上へ伸びます。長文はパネル内でスクロールできます。
- Enter後はパネルを閉じます。コピー・元の入力欄へのフォーカス復帰・貼り付けはバックエンドの専用プロセスが担当します。対象アプリのPasteメニューを実行し、メニューが利用できなければ対象PIDへCmd+Vを送信します。「コピーしました」などの表示は出しません。自動貼り付けにはmacOSのアクセシビリティ許可が必要です。起動時に許可を確認し、Enter時は許可ダイアログを出しません。失敗時もコピーは保持し、メニューバーとログに理由を残します。
- 背景は無彩色の半透明すりガラスです。4pxのレインボーの縁と赤い録音点は一定の明るさで、周囲の4pxの赤いブラーだけが約1.07秒周期で明滅します。「視差効果を減らす」が有効な場合は明滅を止めます。
- 連続録音の結果は録音ごとに保持します。前の最終結果が新しいライブ表示を上書きすることはありません。現在の結果を貼り付けるか閉じると、未処理の結果があれば表示します。空でない結果の履歴は設定ディレクトリの `history.json` に保存します。音声は保存しません。
- GUIはGPUI、BLE受信は別のRustプロセス、音声認識と貼り付けはそれぞれ独立した補助プロセスで動きます。GUIからバックエンドへは標準入力のJSONコマンド、逆方向は標準出力のJSONイベントで通信します。GUI終了時はIPCの切断でバックエンドと子プロセスも終了します。

`build-app.sh`は利用可能なApple Development署名を使います。`CODESIGN_IDENTITY`で指定することもできます。証明書がない場合はアドホック署名になり、再ビルド後にアクセシビリティの再許可が必要になることがあります。

`.app`にはBLE・SpeechAnalyzer・音声読み込み・貼り付けの補助バイナリも含めます。アプリを移動する場合は `.app` 全体を移動してください。macOSの許可を安定して使うため、普段は同じ場所の `.app` を使ってください。

### CLIでの録音・表示

- 保存UUIDに一致する広告を探索して接続します。受信後も接続を維持し、切断されたら探索し直します。
- 起動時は完了済みの古い録音を読み飛ばします。初回接続時に録音中なら、その録音の先頭から受信します。リング内の音声は削除しません。
- BLE転送、録音デコード・認識制御、ライブ認識、録音全体の認識をプロセスで分離しています。
- 受信音声はライブ認識へ逐次入力。録音終了を検出すると暫定結果の表示を停止し、残りの音声を回収します。
- 録音全体が揃ったら、別の認識セッションへ0.5秒単位で連続入力し、最終全文をもう一度表示します。前の録音の最終認識中も、次の録音を受信できます。
- 同じ起動中の切断は、未取得チャンクから再開します。アプリ再起動時は前回の未処理キューを引き継ぎません。
- 通常のBLE使用では音声やチャンクをファイルに保存しません。

TTYではライブ結果を同じ行で更新します。パイプや`--log`使用時は更新された全文を一行ずつ出力します。最終結果はライブ結果と同じ文章でも再度出力します。

待機中もBLE接続を維持します。転送中は250msを目安に、処理中のチャンクが終わった時点でリング状態を確認します。録音終了は非録音状態が250ms以上続くことを再取得で確認して判定します。ボタン解放の瞬間の専用通知ではありません。BLE転送が録音に追いついていない場合、最終結果には残りの転送時間が必要です。

## ログ・CLI

```sh
./target/release/pebble-index                    # 通常起動 = listen
./target/release/pebble-index -v                 # デバッグを標準エラーへ
./target/release/pebble-index --log debug.log    # 全出力をファイルにも追記
./target/release/pebble-index --language en-US
./target/release/pebble-index listen --no-transcribe -v
./target/release/pebble-index fetch --transcribe # 保存済み録音を取得して終了
./target/release/pebble-index inspect
```

`--log`はデバッグ表示も有効にし、標準出力・標準エラーの両方をUTF-8ファイルへ追記します。`-v`の併記は不要です。起動・終了PID、接続・転送時間、認識待ち時間、入力音声長、確定結果の時間区間などを記録します。文字起こし内容もログに入ります。

`--timeout`の既定値は30秒、`--interval`は0.25秒です。通常接続は最大8秒、読み出しは最大5秒、認識コマンド応答は最大30秒で失敗を報告します。`--interval`は受信が追いついた後の待ち時間です。未受信データがある間は固定の待ち時間を挟みません。

## ファイルの文字起こし

WAV、M4A、AIFFなど、AVFoundationがデコードできる音声を扱えます。

```sh
./target/release/pebble-index transcribe recording.m4a --text-output transcript.txt
./target/release/pebble-index transcribe recording.pcm --raw-sample-rate 9997
```

既定言語は`ja-JP`です。`--language en-US`などで変更します。`--wav-output decoded.wav`を指定すると、認識前のデコード済みPCMも書き出せます。通常のメディアは16kHzモノラル、raw PCMは指定されたサンプルレートで出力します。認識への入力時にはBLEと同じ連続ハイパスフィルターを適用します。

## スマホのWebhook

```sh
./target/release/pebble-index serve --host 0.0.0.0 --port 8765 --log webhook.log
```

準備完了時に待受URL・Bearerトークン・保存先を標準出力へ表示します。固定トークンは環境変数`INDEX_WEBHOOK_TOKEN`で指定します。

- URL: `http://<MacのWi-FiまたはTailscaleのIP>:8765/webhook`
- What to send: `Recording`
- Header: `Authorization: Bearer <表示されたトークン>`
- リクエスト署名: オフ
- 録音の送り先: `Webhook only`

Webhookのみ、受信音声・JSON・結果テキストを`recordings/`へ保存します。音声受信はHTTP 202を返してからSpeechAnalyzerで認識し、スマホの文字起こしだけが届いた場合はそのまま保存・表示します。`GET /health`で待受状態を確認できます。保存先は`--output`で変更できます。

## 保存場所

| 内容 | 場所 |
| --- | --- |
| ペアリングUUID | `~/.config/pebble-index-rust/device.json` |
| BLE排他ロック | 同ディレクトリの`bluetooth.lock` |
| 最終認識位置（診断用） | 同ディレクトリの`cursor-*.json` |
| Swiftヘルパー | `~/Library/Caches/pebble-index-rust/rust-*` |
| Apple音声モデル | OSが管理 |

`XDG_CONFIG_HOME`・`XDG_CACHE_HOME`を指定した場合はそちらを使用します。診断用カーソルは通常起動の開始位置には使いません。ロックファイルは残りますが、ロック自体はプロセス終了時にOSが解放します。ファイルを削除する必要はありません。

## アダプター構成

```text
GPUI ── JSON IPC ── Rustバックエンド
                       ├─ desktop_service → Paste.swift
                       └─ 入力元 → 認識ワーカー
                                   ├─ InputAdapter → 共通PCMイベント
                                   └─ 録音管理 → SpeechEngine（live / batch）
```

| 境界 | 実装・役割 |
| --- | --- |
| 入力の変換 | `src/adapters/input/`。`IndexInput`はリングの転送データをデコード・結合し、`PcmInput`はファイルや外部入力のPCMを受け取ります。 |
| 共通音声 | `AudioChunk`は録音ID、モノラルi16サンプル、レート、最終チャンクフラグ、入力元だけが解釈する保存位置を持ちます。 |
| 認識の制御 | `src/recognition.rs`。ライブ認識・録音全体の再認識・結果通知を管理します。BLEや音声圧縮形式、エンジンの起動方法には依存しません。 |
| 認識エンジン | `src/adapters/speech/`の`SpeechEngine`。PCM送信、終了、キャンセル、認識イベントを共通化しています。標準実装はApple SpeechAnalyzerです。 |
| 保存位置 | `InputAdapter::commit`。録音全体の認識完了後、入力アダプターへ保存位置を返します。リングのカーソル保存は入力側が担当します。 |
| 貼り付け | `src/desktop_service.rs`と`native/Paste.swift`。クリップボード、フォーカス復帰、Paste実行を担当します。 |

表示設定は `settings::Presentation`、認識起動計画は `RecognitionPlan`、円形／本文表示の判断は `overlay::presentation` に分離しています。エンジンは `EngineConfig` を受け取り、設定ファイルを直接読みません。入力・録音の結合・全文PCM蓄積は表示設定から独立しています。ライブ無効時もバッチへの全文入力と終了待ちは維持します。コピーはGUIが完了イベントから要求し、専用デスクトップサービスが実行します。

「ライブ変換モード」がオフの場合、履歴を明示的に開いたときだけテキスト欄を表示します。録音・受信・認識中は円形表示、完了時はコピーして閉じます。旧設定の `live_text` は `live_mode` として読み込みます。

BLE切断時は5秒待機して対象録音を中止し、Idleへ戻ります。切断は解放として扱いません。全文認識には、解放の観測と最終音声の両方が必要です。

### 入力デバイスを追加する

外部入力プログラムでマイクや別デバイスの録音APIを扱い、以下のJSONを1行ずつ標準出力へ出して毎回flushしてください。診断メッセージは標準エラーへ出します。外部プログラムは自身で必要なマイク権限を扱います。

```sh
cargo run --release -- stream --input-command /absolute/path/to/input-adapter
# 同じ入力をGUIで使う
INDEX_VOICE_INPUT_COMMAND=/absolute/path/to/input-adapter cargo run --release -- gui
```

```json
{"type":"state","collecting":true}
{"type":"audio","key":"unique-recording-id","rate":16000,"pcm":"BASE64_MONO_S16LE","final":false}
{"type":"state","collecting":false}
{"type":"audio","key":"unique-recording-id","rate":16000,"pcm":"REMAINING_BASE64_PCM","final":true}
```

`state:false`でライブ表示を止め、`final:true`まで残りの音声を受け取ってから全文を認識します。録音IDは毎回変え、同じ録音のレートは固定してください。レートは1,000〜192,000Hz、PCMは符号付き16bit・リトルエンディアン・モノラルです。末尾に残りがなければ空のPCMでも最終チャンクを送れます。完成した録音は`type:recording`で一括投入もできます（`final`不要）。終了前に各録音の最終チャンクを送ってください。独自の圧縮形式を扱う場合は`InputAdapter`を実装し、共通PCMへ変換します。

### 認識モデルを追加する

Rust内で`SpeechEngine`を実装してファクトリーに登録するか、外部プロセスアダプターを指定します。SpeechAnalyzer、Qwen3-ASR MLX、従来CLI用Whisperのアダプターを実装済みです。Qwenの推論実装は`src/qwen/`、常駐ワーカーの入口は`src/bin/qwen_native.rs`です。CLI・GUIと共通のCargo.tomlでビルドします。モデルのセットアップは`src/qwen_setup.rs`、モデル・実行ファイルの準備判定は`src/qwen_runtime.rs`に分離しています。

```sh
INDEX_VOICE_SPEECH_COMMAND=/absolute/path/to/speech-adapter cargo run --release -- gui
```

外部エンジンは引数に言語（例`ja-JP`）と`live`または`batch`を受け取ります。2プロセスが独立して起動します。標準入出力はJSON Lines、PCM送信前に連続DCフィルターを適用します。

- 準備完了：`{"type":"ready"}`を出力。
- 入力：`{"type":"audio","sample_rate":16000,"pcm":"BASE64_MONO_S16LE"}`。受領後に`{"type":"accepted"}`を返す。
- 途中結果：`{"type":"partial","text":"…"}`。
- `{"type":"finish"}`を受けたら`{"type":"final","text":"全文"}`を一度返し、次の録音用セッションを準備する。
- `{"type":"cancel"}`を受けたら停止して`{"type":"cancelled"}`を返す。次の録音へ前の音声を持ち越さない。
- エラー：`{"type":"error","text":"理由"}`。診断ログは標準エラー、補足状況は`{"type":"status","text":"…"}`。

各応答は逐次flushします。外部認識コマンドには30秒の応答制限があります。Qwenと組み込みWhisperの全文認識では録音時間に応じて待機時間を延ばします。実行ファイルのパスを指定し、追加引数が必要ならラッパースクリプトを用意してください。環境変数は起動時に継承されるので、ターミナルから`gui`を起動すると反映されます。外部アダプターの環境変数が指定されている場合は保存設定より優先します。

### 貼り付け権限

起動時のログにある`paste_permission`の`allowed:false`は、OSが自動貼り付けを許可していない状態です。Enter時に許可ダイアログは再表示せず、コピーは残します。設定で許可済みでもログが未許可の場合、古い署名の登録が残っている可能性があります。「プライバシーとセキュリティ → アクセシビリティ」でIndex Voiceを削除し、現在使っている`target/Index Voice.app`を追加して許可した後、アプリを再起動してください。この変更にはmacOSの本人認証が必要になる場合があります。

## 実装・検証

Rust側にTelesto転送・DDRiceデコード・録音結合・連続フィルター・プロセス監視・表示・HTTPサーバーを実装しています。`native/Bluetooth.swift`はCoreBluetoothの橋渡し、`native/AudioDecode.swift`はAVFoundationのデコード、`native/SpeechStream.swift`はSpeechAnalyzerを担当します。必要なソース・依存関係はこのプロジェクト内で完結しています。

```sh
cargo test
cargo clippy --all-targets -- -D warnings

# macOS上のSpeechAnalyzerを実際に使う。リング操作・マイク入力なし。
cargo test --test speech_runtime -- --ignored --nocapture
cargo test --test file_runtime -- --ignored --nocapture

# ダウンロード済みのWhisper large-v3で全文認識・連続ライブ認識を確認
cargo test --test whisper_runtime -- --ignored --nocapture

# macOSのBLE探索を実際に30秒実行。接続・ペアリングは行わない。
cargo test --test bluetooth_runtime -- --ignored --nocapture
```

SpeechAnalyzerについて、20回連続の認識（3分音声を含む）、最初の解放前のライブ出力、全20件の最終結果、0〜180秒の確定区間、子プロセスの終了を確認しています。合成音声は一時ディレクトリ内で作成し、終了時に削除します。

Macの実際のBLE探索でリングを検出し、30秒探索がIPCの待機期限内に完了することを確認しています。Swift側の探索期限には遅延の許容幅を明示したタイマーを使用します。Rust版でのリング接続・ボタン操作から文字起こしまでの実機確認は未実施です。通信ヘルパーのビルド、転送応答の分割・順序・上限、初回録音の開始位置、長い録音・インデックス周回、排他ロック、`pair`の繰り返し実行は自動検証しています。

### タップ操作とジェスチャの拡張フック

`--log` の `button_timing` 行はJSON形式です。`press_duration` は同じ `pid`・`press_id` の押下と解放を対応付け、`observed_down_at`、`observed_up_at`、`observed_hold_ms` を記録します。`down_sample_gap_ms`・`up_sample_gap_ms` は各エッジ周辺の観測間隔で、BLEによる測定の粗さを示します。初回観測ですでに押下中なら `start_unknown`、解放前に切断したら `interrupted_before_up` と記録します。

短押しの `button_collection` 行にはcollection番号・short/long分類とメタデータを残します。取得しているボタンメタデータにはエッジ時刻がないため、`physical_hold_ms` は `null` です。`audio_duration_ms` は音声の長さであり、押下持続時間ではありません。押下エッジを取り逃した短押しについて、音声量や次のタップまでの間隔から押下時間を推定しません。

設定画面でシングルタップとダブルタップに、それぞれ次の動作を割り当てられます。初期設定はシングル＝履歴、ダブル＝ペーストです。

- **履歴を開く**：最新の確定結果を編集できるウィンドウで開きます。上下カーソルで前後の履歴へ移動できます。履歴がない場合は空の編集欄を開きます。
- **現在の入力先へペースト**：現在のクリップボードを、その時点の入力先へ貼り付けます。クリップボードの内容・書式を変更せず、成功時の通知やテキストパネルは出しません。通常のEnter貼り付けと同じアクセシビリティ許可を使います。

極短音声の空結果から自動で編集欄を開く処理は、タップの割り当てに置き換わります。空の結果は履歴・クリップボードを変更しません。

`src/adapters/input/gestures.rs` に、音声認識や録音結合から独立した検出器とフックを置いています。

- デスクトップ入力側の `interaction.rs` が `Idle / Recording / Dictating / Error` を管理します。押下直後は `Idle` のまま候補を保持し、150msの長押し判定と実音声の到着後に録音表示を開始します。通知を取り逃した場合は受信音声の長さでも判定し、判定待ちの先頭音声も認識へ渡します。短押しの空音声は認識や録音表示へ流しません。タップ操作は `Idle` のときだけ確定します。
- 解放通知で直ちに録音表示から受信待ちへ移ります。最終チャンク到着は別イベントで、残りの音声を欠落させず全文認識へ渡します。BLE再接続や遅延チャンクは録音表示を再開しません。状態とトリガの詳細は [デスクトップ状態遷移](docs/desktop-interaction.md) を参照してください。
- `Detector` は `Press::Short` / `Press::Hold` と受信時刻を受け取り、500msの連打待ちの後に `Gesture::SingleTap` / `Gesture::DoubleTap` を確定します。ダブルタップ時にシングルタップを重複発火しません。3連打以上は現在未割り当てで、ダブルタップに丸めません。
- `GestureHook::on_gesture` を実装し、`IndexInput::new` 内の `gesture_hooks.register(...)` に登録すると機能を割り当てられます。現在登録されているのは `LogHook` のみです。
- 検出結果は `InputEvent::Gesture` とJSONの `gesture` イベントでGUIへ渡します。設定の `GestureBindings` で動作を選び、OSへのペーストはデスクトップサービスが実行します。
- フックは入力ワーカー上で呼ばれます。重い処理は別タスクへキューイングしてください。
- Index側ではボタンメタデータと極短音声の判定を併用します。累積したボタン履歴を毎チャンク再発火させず、重複・逆順のcollectionを除外します。起動・切断時は検出状態を破棄し、滞留データの回収後に再開します。fetchでは発火しません。
- 時刻は受信基準です。BLEで配送が遅れた場合、物理的なダブルタップを厳密に保証するものではありません。録音再開の猶予設定とは独立しています。

### 編集と履歴

- Indexの連続録音には標準で **0.5秒の再開猶予** があります。短い入力から長押しへの移行や、離した直後の押し直しは同じ録音へ結合し、最後に全音声をまとめて認識します。猶予中はライブ認識セッションを維持します。
- 猶予はMacで受信した状態通知と録音末尾を基準にします。物理ボタンの厳密な時間ではなく、BLEの転送遅延の影響を受けます。切断後の滞留データは誤結合を避けるため、回収完了まで継続対象から外します。
- `INDEX_VOICE_RESUME_MS=500` で猶予を指定できます（0〜5000、0で無効）。例：アプリを終了してから `INDEX_VOICE_RESUME_MS=500 cargo run --release -- gui`。この機能はIndexのlistenに適用し、マイク・ファイル・fetch入力には適用しません。
- `Continuation join` / `short-to-hold` / `finish` ログに、結合した元の録音IDと確定理由を残します。短押し／長押しは受信情報からの分類であり、物理操作を直接測定したものではありません。
- 日本語IMEのインライン変換、macOS標準の入力ソース切り替え、JISキーボードの英数・かなキーに対応します。変換中のEnterは変換確定、Escは変換操作を優先し、貼り付け・パネル終了は行いません。
- Karabinerで右Command等を `japanese_eisuu` / `japanese_kana` に変換している場合も、編集欄でそのキーを受け取り、登録済みの英語／日本語入力ソースを選択します。元のCommandキーへ独自の機能は割り当てません。`[Index IME]` ログに選択先とmacOS APIの結果を記録します。
- 150ms未満の音声は単押し相当として、認識モデルを呼ばず空文字列で確定します。この閾値より短い実音声も認識しません。Qwenのライブ認識は60ms以上の音量持続を確認してから開始し、押下時の瞬間的な音だけで開始しにくくしています。
- `Esc` はパネルを閉じます。`Enter` は確定結果を貼り付けて閉じます。古い結果が自動で次に現れることはありません。
- 録音・受信・認識中の外側クリックでは処理と表示を継続します。確定後の外側クリックはパネルを閉じます。
- `Ctrl-H` は前の文字を削除、`Ctrl-P` / `Ctrl-N` は上 / 下カーソル移動です。
- 最上段で `↑` / `Ctrl-P` を押すと前の履歴、最下段で `↓` / `Ctrl-N` を押すと次の履歴へ移動します。折り返しを含む本文内では通常のカーソル移動です。IME変換中・範囲選択中は履歴を切り替えません。
- メニューバーの「文字起こし履歴」から最新の確定結果を開けます。選んだ履歴は編集・貼り付け可能で、貼り付け先は履歴を開いたときのアプリです。
- 確定結果と編集内容を最大200件、`~/.config/pebble-index-rust/history.json` に保存します（`XDG_CONFIG_HOME` 指定時はその配下）。音声は保存しません。再起動・リロード後も履歴は残ります。

### On Device（Qwen3-ASR MLX）の実行環境

- モデル: [moona3k/mlx-qwen3-asr-1.7b-8bit](https://huggingface.co/moona3k/mlx-qwen3-asr-1.7b-8bit)（8bit、約2.2 GB）。日本語は`Japanese`を明示して認識します。
- 実装: `src/qwen/` のRust＋MLX C API。mlx-cとMLXのrevisionを固定しています。MLX C++とAppleのMetal等を使用し、Python・uv・PyTorchは実行依存に含めません。[出典とライセンス](licenses/qwen/NOTICE)。
- 保存先: `~/Library/Application Support/Index Voice/qwen-mlx/model/`。既存の検証済み重みを再利用します。旧`.venv`は参照せず、削除もしません。
- `.app`には`QwenNative`、`mlx.metallib`、ライセンスを同梱します。実行時にリポジトリやコンパイラーは不要です。開発時は`INDEX_VOICE_QWEN_BINARY`で隣に`mlx.metallib`がある実行ファイルを指定できます。
- 初回移行時も`setup-qwen`を実行してください。既存ファイルのSHA-256を確認し、`native-model.json`を原子的に更新します。準備判定ではモデルのrevision、各ファイルのサイズ・更新時刻、ネイティブ実行ファイルとMetalリソースを確認します。
- `setup-qwen`は排他ロック付きで再実行可能。モデルのリビジョンを固定し、重み・設定・トークナイザーのSHA-256を確認します。ダウンロードはcurl、認識は完全ローカルです。
- 過去の`parakeet_mlx` / `nemotron_mlx`設定もOn Deviceへ読み替えます。旧モデルの保存済みファイルは削除しませんが、現在の認識では使いません。
- 受信・ACKと推論を分離し、キャンセル前の結果は録音ID・世代番号で破棄します。受領済みPCMと認識に反映済みのPCMを別々に数え、最終結果が全入力を覆うことを確認します。録音ごとにストリーミング状態とリサンプラーを作り直します。
- ライブ認識では冒頭の無音を保留し、発話の200 ms前からモデルへ渡します。発話開始後の間や語尾は削りません。非常に小さい声は開始判定が遅れる場合があります。
- BLE転送中の状態照会の重複と、転送直後の固定待ちを除去しています。録音終了後の回収中は状態照会を1秒間隔に抑えます。音声の生成速度がBLEの実効転送速度を上回る場合、リング側に転送待ちが残るため、認識エンジンだけでは遅延を解消できません。
- 選定理由・日本語精度とストリーミング方式の比較は[ASRモデル比較](docs/asr-model-selection.md)を参照してください。
