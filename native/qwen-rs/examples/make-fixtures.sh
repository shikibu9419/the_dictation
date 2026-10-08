#!/bin/sh
# Generate Japanese reference audio silently. Never play it or open a window.
set -eu
DEST=${1:?usage: make-fixtures.sh OUTPUT_DIRECTORY}
mkdir -p "$DEST"
say -v Kyoko -o "$DEST/short.aiff" 'こんにちは。これは音声認識の動作確認です。'
say -v Kyoko -o "$DEST/long.aiff" '最初の確認です。今日は指輪から届いた音声を、日本語の文章に変換する仕組みについて説明します。ボタンを押すと音声の収集が始まり、話している間は文字が少しずつ表示されます。ボタンを離した後も、届くのが遅れた音声を最後まで受け取ります。短い休憩を挟んだ場合でも、文章の途中が消えてしまわないようにします。次の録音を始めた時には、前の会話の内容を引き継がず、新しい文章として処理します。これで最後の確認を終わります。'
for name in short long; do
    afconvert -f WAVE -d LEI16@16000 -c 1 "$DEST/$name.aiff" "$DEST/$name.wav"
done
