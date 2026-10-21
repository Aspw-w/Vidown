# Vidown App

YouTube / Twitter(X) / Instagram の動画を、画質を選んで保存するデスクトップアプリです（Rust 版）。

## 必要なもの

- [yt-dlp](https://github.com/yt-dlp/yt-dlp) — `Vidown.exe` と同じフォルダに `yt-dlp.exe` を置くか、PATH に追加してください
- ffmpeg（任意）— 映像と音声を結合する形式を選ぶ場合に必要です
- Node.js / Deno など（任意）— YouTube の抽出に使われます

## ビルド

```bash
cargo build --release
```

`target/release/Vidown.exe` が生成されます（アイコン埋め込み済み）。

## 設定の保存

保存先フォルダの設定は `%APPDATA%\Vidown\settings.json` に保存され、次回起動時に読み込まれます。

## 構成

| ファイル | 内容 |
| --- | --- |
| `src/app.rs` | 画面（egui）と画質選択ダイアログ |
| `src/worker.rs` | 画質一覧の取得とダウンロード処理 |
| `src/ytdlp.rs` | yt-dlp の起動とオプション |
| `src/parallel_http.rs` | 単一ファイル HTTP ストリームの並列レンジダウンロード |
| `src/progress.rs` | 映像・音声ストリームの進捗集計 |
| `src/formats.rs` | 画質一覧の組み立てと並べ替え |
| `src/settings.rs` | 保存先フォルダ設定の読み書き |
