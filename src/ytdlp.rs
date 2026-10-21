use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::Value;

use crate::progress::ProgressData;
use crate::util::{detect_js_runtime, ffmpeg_available};

pub const FRAGMENT_DOWNLOAD_WORKERS: u32 = 16;
pub const DOWNLOAD_BUFFER_SIZE: usize = 1024 * 1024;
pub const NATIVE_HTTP_CHUNK_SIZE: usize = 10 * 1024 * 1024;

const PROGRESS_PREFIX: &str = "VIDOWN_PROGRESS ";
const PROGRESS_TEMPLATE: &str =
    "download:VIDOWN_PROGRESS %(info.{format_id,filesize,filesize_approx})j\t%(progress)j";
const OUTPUT_TEMPLATE: &str = "%(title).200B [%(id)s].%(ext)s";
const CHROME_COOKIE_COPY_ERROR: &str = "Could not copy Chrome cookie database";

pub struct YdlOptions {
    args: Vec<OsString>,
    pub ffmpeg_available: bool,
    pub js_runtime_name: Option<&'static str>,
}

/// Command-line equivalent of the yt-dlp option dict the original app passed to `YoutubeDL`.
pub fn make_ydl_options(
    platform: &str,
    browser_name: Option<&str>,
    cookie_file_path: Option<&Path>,
    output_dir: Option<&Path>,
    format_selector: Option<&str>,
    log: impl Fn(String),
) -> YdlOptions {
    let ffmpeg_available = ffmpeg_available();
    let mut args: Vec<OsString> = [
        "--ignore-config",
        "--no-playlist",
        "--windows-filenames",
        "--no-colors",
        "--retries",
        "10",
        "--fragment-retries",
        "10",
        "--extractor-retries",
        "5",
        "--file-access-retries",
        "3",
        "--socket-timeout",
        "30",
        "--no-resize-buffer",
        "--downloader",
        "m3u8:native",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push("--buffer-size".into());
    args.push(DOWNLOAD_BUFFER_SIZE.to_string().into());
    args.push("--http-chunk-size".into());
    args.push(NATIVE_HTTP_CHUNK_SIZE.to_string().into());
    args.push("--concurrent-fragments".into());
    args.push(FRAGMENT_DOWNLOAD_WORKERS.to_string().into());

    if let Some(output_dir) = output_dir {
        args.push("--output".into());
        args.push(output_dir.join(OUTPUT_TEMPLATE).into_os_string());
    }

    if ffmpeg_available {
        args.push("--merge-output-format".into());
        args.push("mp4/mkv".into());
    }

    if let Some(format_selector) = format_selector {
        args.push("--format".into());
        args.push(format_selector.into());
    }

    if let Some(cookie_file_path) = cookie_file_path {
        args.push("--cookies".into());
        args.push(cookie_file_path.as_os_str().to_owned());
    } else if let Some(browser_name) = browser_name {
        args.push("--cookies-from-browser".into());
        args.push(browser_name.into());
    }

    let mut js_runtime_name = None;
    if platform == "YouTube" {
        if let Some((runtime_name, runtime_path)) = detect_js_runtime() {
            let mut runtime_arg = OsString::from(format!("{runtime_name}:"));
            runtime_arg.push(runtime_path.as_os_str());
            args.push("--js-runtimes".into());
            args.push(runtime_arg);
            args.push("--remote-components".into());
            args.push("ejs:github".into());
            js_runtime_name = Some(runtime_name);
        } else {
            log("YouTube 抽出用の JavaScript ランタイムが見つかりません。 Node.js か Deno がない環境では一部動画が失敗する場合があります。".to_string());
        }
    }

    YdlOptions {
        args,
        ffmpeg_available,
        js_runtime_name,
    }
}

pub fn format_download_error(message: &str) -> String {
    if message.contains(CHROME_COOKIE_COPY_ERROR) {
        return "Chrome/EdgeのCookieデータベースをコピーできませんでした。 Chrome/Edgeを完全に終了して再試行するか、Netscape形式のcookies.txtをCookieファイルに指定してください。 年齢制限付きYouTubeはログイン済みアカウントのCookieが必要です。".to_string();
    }
    message.to_string()
}

pub struct YtDlp {
    path: PathBuf,
}

impl YtDlp {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn spawn(&self, args: &[OsString]) -> Result<Child, String> {
        let mut command = Command::new(&self.path);
        command
            .args(args)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command.spawn().map_err(|err| format!("yt-dlp を起動できませんでした: {err}"))
    }

    /// Runs yt-dlp to completion while forwarding stdout lines, returning an error built from stderr on failure.
    fn run(&self, args: &[OsString], log: &(impl Fn(String) + Sync), mut on_stdout_line: impl FnMut(&str)) -> Result<(), String> {
        let mut child = self.spawn(args)?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (status, last_error) = std::thread::scope(|scope| {
            let stderr_reader = scope.spawn(move || {
                let mut last_error: Option<String> = None;
                let mut last_line: Option<String> = None;
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let clean = line.trim();
                    if clean.is_empty() {
                        continue;
                    }
                    if let Some(warning) = clean.strip_prefix("WARNING: ") {
                        log(format!("Warning: {warning}"));
                    } else if clean.starts_with("ERROR: ") {
                        log(format!("Error: {clean}"));
                        last_error = Some(clean.to_string());
                    } else {
                        log(clean.to_string());
                    }
                    last_line = Some(clean.to_string());
                }
                last_error.or(last_line)
            });

            let mut reader = BufReader::new(stdout);
            let mut buffer = Vec::new();
            loop {
                buffer.clear();
                match reader.read_until(b'\n', &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buffer);
                        on_stdout_line(line.trim_end_matches(['\r', '\n']));
                    }
                }
            }
            let status = child.wait();
            (status, stderr_reader.join().ok().flatten())
        });

        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(last_error.unwrap_or_else(|| {
                format!("yt-dlp がエラーで終了しました (終了コード {})", status.code().unwrap_or(-1))
            })),
            Err(err) => Err(format!("yt-dlp の終了を待機できませんでした: {err}")),
        }
    }

    fn run_json(&self, args: Vec<OsString>, log: &(impl Fn(String) + Sync)) -> Result<Value, String> {
        let mut json_line = None;
        self.run(&args, log, |line| {
            if line.starts_with('{') {
                json_line = Some(line.to_string());
            }
        })?;
        let line = json_line.ok_or_else(|| "ダウンロード情報を取得できませんでした。".to_string())?;
        serde_json::from_str(&line).map_err(|err| format!("yt-dlp の出力を解析できませんでした: {err}"))
    }

    /// `extract_info(url, download=False)` with yt-dlp's default format selection.
    pub fn extract_info(&self, options: &YdlOptions, url: &str, log: &(impl Fn(String) + Sync)) -> Result<Value, String> {
        let mut args = options.args.clone();
        args.extend(["--dump-single-json".into(), "--".into(), url.into()]);
        self.run_json(args, log)
    }

    /// Like `extract_info`, but also resolves the selected format(s) and the output `filename`.
    pub fn resolve_download(&self, options: &YdlOptions, url: &str, log: &(impl Fn(String) + Sync)) -> Result<Value, String> {
        let mut args = options.args.clone();
        args.extend(["--print".into(), "%()j".into(), "--".into(), url.into()]);
        self.run_json(args, log)
    }

    /// `process_info(info)`: downloads and post-processes a previously resolved info dict.
    pub fn download_info(
        &self,
        options: &YdlOptions,
        info_json_path: &Path,
        log: &(impl Fn(String) + Sync),
        mut on_progress: impl FnMut(ProgressData),
    ) -> Result<(), String> {
        let mut args = options.args.clone();
        args.extend([
            "--quiet".into(),
            "--progress".into(),
            "--newline".into(),
            "--progress-template".into(),
            PROGRESS_TEMPLATE.into(),
            "--load-info-json".into(),
            info_json_path.as_os_str().to_owned(),
        ]);
        self.run(&args, log, |line| {
            let Some(payload) = line.strip_prefix(PROGRESS_PREFIX) else {
                if !line.trim().is_empty() {
                    log(line.trim().to_string());
                }
                return;
            };
            let Some((info_text, progress_text)) = payload.split_once('\t') else {
                return;
            };
            let info = serde_json::from_str(info_text).unwrap_or(Value::Null);
            if let Ok(progress) = serde_json::from_str::<Value>(progress_text) {
                on_progress(ProgressData::from_json(info, &progress));
            }
        })
    }
}
