# Vidown App

YouTube / Twitter(X) / Instagram の動画を、画質を選んで保存するデスクトップアプリです（Rust 版）。

## ビルド

```bat
build.bat
```

`dist\Vidown.exe` が生成されます。 yt-dlp（nightly）・ffmpeg・Deno を埋め込んだ単体の exe なので、依存パッケージや Visual C++ ランタイムが入っていない PC でもこの exe だけで動作します。

- 初回起動時に埋め込みツールを `%LOCALAPPDATA%\Vidown\tools\` に展開します
- `build.bat` は毎回最新の yt-dlp nightly を取得し、ffmpeg・Deno は `bundled\` にキャッシュします。 すべて取り直す場合は `build.bat update`
- YouTube の「The page needs to be reloaded」エラー（yt-dlp#17389 / #17405）は nightly 版で修正済みです。 それでも発生した場合は `player_client=default,web_embedded,-tv_downgraded` で自動的に再試行します

### ツールを埋め込まないビルド

```bash
cargo build --release
```

`bundled\` フォルダが無い場合は何も埋め込まず、従来どおり以下を探して使います。

- [yt-dlp](https://github.com/yt-dlp/yt-dlp) — `Vidown.exe` と同じフォルダの `yt-dlp.exe`、または PATH
- ffmpeg（任意）— 映像と音声を結合する形式を選ぶ場合に必要です
- Node.js / Deno など（任意）— YouTube の抽出に使われます

## 設定の保存

保存先フォルダの設定は `%APPDATA%\Vidown\settings.json` に保存され、次回起動時に読み込まれます。

## 構成

| ファイル | 内容 |
| --- | --- |
| `src/app.rs` | 画面（egui）と画質選択ダイアログ |
| `src/worker.rs` | 画質一覧の取得とダウンロード処理 |
| `src/ytdlp.rs` | yt-dlp の起動とオプション |
| `src/bundled.rs` | 埋め込みツールの展開 |
| `src/parallel_http.rs` | 単一ファイル HTTP ストリームの並列レンジダウンロード |
| `src/progress.rs` | 映像・音声ストリームの進捗集計 |
| `src/formats.rs` | 画質一覧の組み立てと並べ替え |
| `src/settings.rs` | 保存先フォルダ設定の読み書き |
