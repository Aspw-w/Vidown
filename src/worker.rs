use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::formats::{FormatChoice, build_format_choices};
use crate::parallel_http::{self, Outcome, can_use_parallel_http};
use crate::progress::{DownloadProgressTracker, ProgressData};
use crate::util::{format_eta, human_readable_size, json_str};
use crate::ytdlp::{YtDlp, format_download_error, make_ydl_options};

pub enum Event {
    Log(String),
    Progress { progress: Option<f64>, message: String },
    FormatsLoaded(FormatsLoaded),
    Done { success: bool, message: String },
}

#[derive(Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub output_dir: PathBuf,
    pub browser_name: Option<&'static str>,
    pub cookie_file_path: Option<PathBuf>,
    pub platform: &'static str,
}

pub struct FormatsLoaded {
    pub title: String,
    pub request: DownloadRequest,
    pub choices: Vec<FormatChoice>,
}

#[derive(Clone)]
pub struct Emitter {
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl Emitter {
    pub fn new(tx: Sender<Event>, ctx: eframe::egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn emit(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }

    pub fn log(&self, message: impl Into<String>) {
        let message = message.into();
        let clean = message.trim();
        if !clean.is_empty() {
            self.emit(Event::Log(clean.to_string()));
        }
    }
}

pub fn fetch_available_formats(ytdlp: &YtDlp, emitter: &Emitter, request: DownloadRequest) {
    let log = |message: String| emitter.log(message);
    let options = make_ydl_options(
        request.platform,
        request.browser_name,
        request.cookie_file_path.as_deref(),
        None,
        None,
        log,
    );

    if options.ffmpeg_available {
        emitter.log("ffmpeg を検出しました。結合が必要な形式も選択できます。");
    } else {
        emitter.log("ffmpeg が見つからないため、音声入りの単体形式のみ一覧に出します。");
    }

    if request.cookie_file_path.is_some() {
        emitter.log("Cookieファイルを使用してアクセスします。");
    } else if let Some(browser_name) = request.browser_name {
        emitter.log(format!("{browser_name} のCookieを使用してアクセスします。"));
    }

    if let Some(runtime_name) = options.js_runtime_name {
        emitter.log(format!("YouTube 抽出用に {runtime_name} を使用します。"));
    }

    let result = ytdlp.extract_info(&options, &request.url, &log).and_then(|info| {
        let title = [info.get("title"), info.get("id")]
            .into_iter()
            .map(json_str)
            .find(|text| !text.is_empty())
            .unwrap_or_else(|| "video".to_string());
        let choices = build_format_choices(&info, request.platform, options.ffmpeg_available);
        if choices.is_empty() {
            return Err("選択可能な動画形式を取得できませんでした。".to_string());
        }
        Ok((title, choices))
    });

    match result {
        Ok((title, choices)) => emitter.emit(Event::FormatsLoaded(FormatsLoaded { title, request, choices })),
        Err(message) => emitter.emit(Event::Done {
            success: false,
            message: format_download_error(&message),
        }),
    }
}

pub fn download_selected_format(
    ytdlp: &YtDlp,
    emitter: &Emitter,
    tracker: &Arc<DownloadProgressTracker>,
    request: DownloadRequest,
    choice: FormatChoice,
) {
    let log = |message: String| emitter.log(message);
    let options = make_ydl_options(
        request.platform,
        request.browser_name,
        request.cookie_file_path.as_deref(),
        Some(&request.output_dir),
        Some(&choice.selector),
        log,
    );

    let result = ytdlp.resolve_download(&options, &request.url, &log).and_then(|info| {
        if !info.is_object() {
            return Err("ダウンロード情報を取得できませんでした。".to_string());
        }

        let initial_progress = tracker.prepare(&info);
        let total_label = match initial_progress.total_bytes {
            Some(total) => format!(
                "{}{}",
                if initial_progress.total_is_estimate { "約 " } else { "" },
                human_readable_size(Some(total as f64))
            ),
            None => "確認中".to_string(),
        };
        emitter.emit(Event::Progress {
            progress: initial_progress.progress,
            message: format!("ダウンロードを開始します... 合計 {total_label}"),
        });

        process_info(ytdlp, emitter, tracker, &options, &request, &info)
    });

    match result {
        Ok(()) => emitter.emit(Event::Done {
            success: true,
            message: "ダウンロードが完了しました。".to_string(),
        }),
        Err(message) => emitter.emit(Event::Done {
            success: false,
            message: format_download_error(&message),
        }),
    }
}

fn process_info(
    ytdlp: &YtDlp,
    emitter: &Emitter,
    tracker: &DownloadProgressTracker,
    options: &crate::ytdlp::YdlOptions,
    request: &DownloadRequest,
    info: &Value,
) -> Result<(), String> {
    let report = |data: ProgressData| on_progress(tracker, emitter, &data);
    let log = |message: String| emitter.log(message);

    if can_use_parallel_http(info) {
        emitter.log("Invoking parallel http downloader on a direct HTTP stream");
        if let Outcome::Completed =
            parallel_http::download(info, request.cookie_file_path.as_deref(), &log, &report)
        {
            return Ok(());
        }
    }

    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    let info_json_path = std::env::temp_dir().join(format!("vidown-{}-{nanos}.info.json", std::process::id()));
    let serialized = serde_json::to_vec(info).map_err(|err| err.to_string())?;
    std::fs::write(&info_json_path, serialized).map_err(|err| err.to_string())?;
    let result = ytdlp.download_info(options, &info_json_path, &log, report);
    let _ = std::fs::remove_file(&info_json_path);
    result
}

fn on_progress(tracker: &DownloadProgressTracker, emitter: &Emitter, data: &ProgressData) {
    let status = data.status.as_str();
    if matches!(status, "downloading" | "finished" | "error") {
        let snapshot = tracker.update(data);
        if !snapshot.should_emit {
            return;
        }

        let downloaded_text = human_readable_size(Some(snapshot.downloaded_bytes as f64));
        let (amount_text, percent_text) = match snapshot.total_bytes {
            None => (format!("{downloaded_text} / 合計サイズ確認中"), "計測中".to_string()),
            Some(total) => {
                let approximate = if snapshot.total_is_estimate { "約 " } else { "" };
                let percent_value = snapshot.progress.unwrap_or(0.0) * 100.0;
                (
                    format!("{downloaded_text} / {approximate}{}", human_readable_size(Some(total as f64))),
                    format!("{approximate}{percent_value:5.1}%"),
                )
            }
        };

        let status_text = if snapshot.all_finished {
            format!("ダウンロード完了。{amount_text}  後処理を実行しています...")
        } else if status == "error" {
            format!("ダウンロードを再試行しています... {amount_text}")
        } else {
            let speed_text = if snapshot.speed > 0.0 {
                format!("{}/s", human_readable_size(Some(snapshot.speed)))
            } else {
                "-- B/s".to_string()
            };
            format!(
                "ダウンロード中... {percent_text}  {amount_text}  {speed_text}  ETA {}",
                format_eta(snapshot.eta)
            )
        };

        emitter.emit(Event::Progress {
            progress: snapshot.progress,
            message: status_text,
        });
    }

    if status == "finished" && !data.filename.is_empty() {
        emitter.log(format!("保存ファイル: {}", data.filename));
    }
}
