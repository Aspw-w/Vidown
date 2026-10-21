use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::cookies::CookieJar;
use crate::progress::{PROGRESS_EVENT_INTERVAL, ProgressData};
use crate::util::{is_http_url, json_str, json_truthy};
use crate::ytdlp::DOWNLOAD_BUFFER_SIZE;

pub const PARALLEL_HTTP_WORKERS: usize = 8;
const PARALLEL_HTTP_MIN_FILE_SIZE: u64 = 8 * 1024 * 1024;
const PARALLEL_HTTP_CHUNK_SIZE: u64 = 4 * 1024 * 1024;
const RETRY_COUNT: u32 = 10;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Outcome {
    Completed,
    Fallback,
}

pub fn can_use_parallel_http(info: &Value) -> bool {
    let protocol = json_str(info.get("protocol"));
    let url = json_str(info.get("url"));
    let filename = json_str(info.get("filename"));
    !filename.is_empty()
        && filename != "-"
        && (protocol == "http" || protocol == "https")
        && is_http_url(&url)
        && !json_truthy(info.get("fragments"))
        && !json_truthy(info.get("requested_formats"))
        && !json_truthy(info.get("section_start"))
        && !json_truthy(info.get("section_end"))
}

fn parse_http_range(header: Option<&str>) -> (Option<u64>, Option<u64>, Option<u64>) {
    let Some(header) = header else {
        return (None, None, None);
    };
    let Some(spec) = header.trim().strip_prefix("bytes").map(str::trim_start) else {
        return (None, None, None);
    };
    let spec = spec.strip_prefix('=').unwrap_or(spec).trim();
    let (range, total) = spec.split_once('/').unwrap_or((spec, ""));
    let (start, end) = range.split_once('-').unwrap_or((range, ""));
    (
        start.trim().parse().ok(),
        end.trim().parse().ok(),
        total.trim().parse().ok(),
    )
}

fn content_range(response: &Response) -> (Option<u64>, Option<u64>, Option<u64>) {
    parse_http_range(
        response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
    )
}

fn build_headers(info: &Value, url: &str, cookie_file_path: Option<&Path>) -> Option<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(reqwest::header::ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Some(http_headers) = info.get("http_headers").and_then(Value::as_object) {
        for (name, value) in http_headers {
            let (Ok(name), Some(Ok(value))) = (
                HeaderName::from_bytes(name.as_bytes()),
                value.as_str().map(HeaderValue::from_str),
            ) else {
                continue;
            };
            headers.insert(name, value);
        }
    }
    if headers.contains_key(reqwest::header::RANGE) {
        return None;
    }
    if let Some(cookie_file_path) = cookie_file_path {
        let jar = CookieJar::load(cookie_file_path).ok()?;
        if let Some(cookie) = jar.header_for(url).and_then(|cookie| HeaderValue::from_str(&cookie).ok()) {
            headers.insert(reqwest::header::COOKIE, cookie);
        }
    }
    Some(headers)
}

fn probe_size(client: &Client, url: &str, headers: &HeaderMap) -> Option<(u64, Option<String>)> {
    let response = client
        .get(url)
        .headers(headers.clone())
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    let (range_start, _range_end, total) = content_range(&response);
    if range_start != Some(0) {
        return None;
    }
    let total = total.filter(|total| *total > 0)?;
    let last_modified = response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Some((total, last_modified))
}

struct RangeDownload<'a> {
    client: &'a Client,
    url: &'a str,
    headers: &'a HeaderMap,
    tmpfilename: &'a Path,
    total_bytes: u64,
    stop: &'a AtomicBool,
    progress: &'a ProgressCounter<'a>,
}

impl RangeDownload<'_> {
    fn download_range(&self, range_start: u64, range_end: u64) -> Result<(), String> {
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.tmpfilename)
            .map_err(|err| err.to_string())?;
        let mut position = range_start;
        let mut attempts = 0;
        let mut buffer = vec![0u8; DOWNLOAD_BUFFER_SIZE];

        while position <= range_end && !self.stop.load(Ordering::Relaxed) {
            match self.fetch_into(&mut output, &mut position, range_end, &mut buffer) {
                Ok(()) => attempts = 0,
                Err(err) => {
                    attempts += 1;
                    if attempts > RETRY_COUNT || self.stop.load(Ordering::Relaxed) {
                        return Err(err);
                    }
                    let delay = (0.25 * 2f64.powi(attempts as i32 - 1)).min(2.0);
                    std::thread::sleep(Duration::from_secs_f64(delay));
                }
            }
        }
        Ok(())
    }

    fn fetch_into(&self, output: &mut File, position: &mut u64, range_end: u64, buffer: &mut [u8]) -> Result<(), String> {
        let mut response = self
            .client
            .get(self.url)
            .headers(self.headers.clone())
            .header(reqwest::header::RANGE, format!("bytes={position}-{range_end}"))
            .send()
            .and_then(Response::error_for_status)
            .map_err(|err| err.to_string())?;

        let (response_start, response_end, response_total) = content_range(&response);
        let Some(response_end) = response_end.filter(|end| {
            response_start == Some(*position) && *end <= range_end && response_total == Some(self.total_bytes)
        }) else {
            return Err("サーバーが要求したバイト範囲を返しませんでした".to_string());
        };

        output.seek(SeekFrom::Start(*position)).map_err(|err| err.to_string())?;
        let mut remaining = response_end - *position + 1;
        while remaining > 0 && !self.stop.load(Ordering::Relaxed) {
            let want = remaining.min(buffer.len() as u64) as usize;
            let read = response.read(&mut buffer[..want]).map_err(|err| err.to_string())?;
            if read == 0 {
                return Err("バイト範囲の受信が途中で終了しました".to_string());
            }
            output.write_all(&buffer[..read]).map_err(|err| err.to_string())?;
            *position += read as u64;
            remaining -= read as u64;
            self.progress.add(read as u64, false);
        }
        Ok(())
    }
}

struct ProgressCounter<'a> {
    state: Mutex<(u64, Option<Instant>)>,
    started_at: Instant,
    total_bytes: u64,
    filename: &'a Path,
    tmpfilename: &'a Path,
    info: &'a Value,
    on_progress: &'a (dyn Fn(ProgressData) + Sync),
}

impl ProgressCounter<'_> {
    fn add(&self, delta: u64, force: bool) {
        let now = Instant::now();
        let current_downloaded = {
            let mut state = self.state.lock().unwrap();
            state.0 += delta;
            if !force && state.1.is_some_and(|last| now.duration_since(last) < PROGRESS_EVENT_INTERVAL) {
                return;
            }
            state.1 = Some(now);
            state.0
        };

        let elapsed = now.duration_since(self.started_at).as_secs_f64().max(0.001);
        (self.on_progress)(ProgressData {
            status: "downloading".to_string(),
            downloaded_bytes: Some(current_downloaded),
            total_bytes: Some(self.total_bytes),
            total_bytes_estimate: None,
            speed: Some(current_downloaded as f64 / elapsed).filter(|speed| *speed > 0.0),
            filename: self.filename.to_string_lossy().into_owned(),
            tmpfilename: self.tmpfilename.to_string_lossy().into_owned(),
            ctx_id: String::new(),
            info: self.info.clone(),
        });
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn finished(filename: &Path, total_bytes: u64, info: &Value) -> ProgressData {
    ProgressData {
        status: "finished".to_string(),
        downloaded_bytes: Some(total_bytes),
        total_bytes: Some(total_bytes),
        filename: filename.to_string_lossy().into_owned(),
        info: info.clone(),
        ..Default::default()
    }
}

pub fn download(
    info: &Value,
    cookie_file_path: Option<&Path>,
    log: &(dyn Fn(String) + Sync),
    on_progress: &(dyn Fn(ProgressData) + Sync),
) -> Outcome {
    let filename = PathBuf::from(json_str(info.get("filename")));
    let url = json_str(info.get("url"));
    if !json_str(info.get("impersonate")).is_empty() || json_truthy(info.get("request_data")) {
        return Outcome::Fallback;
    }

    if let Ok(metadata) = std::fs::metadata(&filename) {
        log(format!("{} has already been downloaded", filename.display()));
        on_progress(finished(&filename, metadata.len(), info));
        return Outcome::Completed;
    }

    let native_tmpfilename = path_with_suffix(&filename, ".part");
    if native_tmpfilename.exists() {
        return Outcome::Fallback;
    }
    let tmpfilename = path_with_suffix(&native_tmpfilename, ".parallel");
    let _ = std::fs::remove_file(&tmpfilename);

    let Some(headers) = build_headers(info, &url, cookie_file_path) else {
        return Outcome::Fallback;
    };
    let Ok(client) = Client::builder()
        .connect_timeout(SOCKET_TIMEOUT)
        .timeout(None::<Duration>)
        .build()
    else {
        return Outcome::Fallback;
    };

    let Some((total_bytes, last_modified)) = probe_size(&client, &url, &headers) else {
        return Outcome::Fallback;
    };
    if total_bytes < PARALLEL_HTTP_MIN_FILE_SIZE {
        return Outcome::Fallback;
    }

    let target_range_size = PARALLEL_HTTP_CHUNK_SIZE.max(total_bytes.div_ceil(PARALLEL_HTTP_WORKERS as u64 * 4));
    let ranges: Vec<(u64, u64)> = (0..total_bytes)
        .step_by(target_range_size as usize)
        .map(|start| (start, (start + target_range_size - 1).min(total_bytes - 1)))
        .collect();
    if ranges.len() < 2 {
        return Outcome::Fallback;
    }
    let worker_count = PARALLEL_HTTP_WORKERS.min(ranges.len());

    if let Some(parent) = filename.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let prepared = File::create(&tmpfilename).and_then(|file| file.set_len(total_bytes));
    if prepared.is_err() {
        let _ = std::fs::remove_file(&tmpfilename);
        return Outcome::Fallback;
    }
    log(format!("[download] Destination: {}", filename.display()));

    let stop = AtomicBool::new(false);
    let next_range = AtomicUsize::new(0);
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let started_at = Instant::now();
    let progress = ProgressCounter {
        state: Mutex::new((0, None)),
        started_at,
        total_bytes,
        filename: &filename,
        tmpfilename: &tmpfilename,
        info,
        on_progress,
    };
    let range_download = RangeDownload {
        client: &client,
        url: &url,
        headers: &headers,
        tmpfilename: &tmpfilename,
        total_bytes,
        stop: &stop,
        progress: &progress,
    };

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let Some(&(range_start, range_end)) = ranges.get(next_range.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if let Err(err) = range_download.download_range(range_start, range_end) {
                        failures.lock().unwrap().push(err);
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        let _ = std::fs::remove_file(&tmpfilename);
        log(format!("Warning: {}", failures[0]));
        log("Warning: 並列HTTPダウンロードを継続できないため、標準方式で再試行します。".to_string());
        return Outcome::Fallback;
    }

    progress.add(0, true);
    if std::fs::rename(&tmpfilename, &filename).is_err() {
        let _ = std::fs::remove_file(&tmpfilename);
        return Outcome::Fallback;
    }
    if let Some(modified) = last_modified.as_deref().and_then(|value| httpdate::parse_http_date(value).ok()) {
        let _ = filetime::set_file_mtime(&filename, filetime::FileTime::from_system_time(modified));
    }

    on_progress(finished(&filename, total_bytes, info));
    Outcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(parse_http_range(Some("bytes 0-0/1234")), (Some(0), Some(0), Some(1234)));
        assert_eq!(parse_http_range(Some("bytes 10-99/*")), (Some(10), Some(99), None));
        assert_eq!(parse_http_range(None), (None, None, None));
    }
}
