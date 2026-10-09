# Pebble Jev

Index 01 リングのボタンを押している間の音声を OpenAI Realtime API（`gpt-realtime-2.1`）へ送り、返ってきた音声と文字起こし、`add_todo` / `add_memo` の tool call 結果をフローティングパネルにストリーミング表示する macOS アプリです。

## 準備

1. API キーを環境変数 `OPENAI_API_KEY` か `~/.config/pebble-jev/settings.json` の `api_key` に設定します。
2. リングを登録します（dictation アプリと同じ `~/.config/pebble-index-rust/device.json` を共有するので、どちらかで一度登録すれば十分です）。

```sh
cd desktop/rust
cargo run --release -p pebble-jev -- pair
```

## 起動

```sh
OPENAI_API_KEY=sk-... cargo run --release -p pebble-jev
# または
sh apps/pebble-jev/build-app.sh && open "target/Pebble Jev.app"
```

- リングを長押しすると丸いパネルにマイクが出て、BLE で届く音声チャンク（9997 Hz）を 24 kHz にリサンプリングしながら `input_audio_buffer.append` で送ります。
- 離して最後のパートが届くと `commit` → `response.create` し、ユーザーの文字起こし・応答テキストが流れ、応答音声を再生します。100 ms 未満の録音は送らずに破棄します。
- 応答中に再び押すと再生を止め、聞こえた位置まで `conversation.item.truncate` してから次の発話を受け付けます。シングルタップは応答の停止です。
- 「牛乳を買うを TODO に追加して」のように頼むと tool call のカード（名前・引数・結果）が出て、`~/.config/pebble-jev/todos.json` / `memos.json` に追記されます。
- `Esc` か外側クリックでパネルを閉じます。メニューバーの「設定ファイルを開く…」で `settings.json` を編集し、「リロード」で反映します。

## 設定

`~/.config/pebble-jev/settings.json`

| キー | 既定 | 内容 |
| --- | --- | --- |
| `api_key` | なし | 未設定なら `OPENAI_API_KEY` |
| `model` | `gpt-realtime-2.1` | Realtime モデル |
| `voice` | `marin` | 応答音声 |
| `language` | `ja` | ユーザー音声の文字起こし言語 |
| `instructions` | 日本語の短い指示 | system プロンプト |
| `reception` | dictation と同じ既定 | リングの受信タイミング |

## 検証

```sh
cargo test -p pebble-jev
cargo clippy -p pebble-jev --all-targets -- -D warnings
```

リング・Bluetooth・API 接続を伴う動作は実機で確認してください。`--verbose` で送受信ログを stderr に出します。
