use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::util::{json_str, nonnegative_int};

pub const PROGRESS_EVENT_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, Default)]
pub struct ProgressData {
    pub status: String,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub total_bytes_estimate: Option<u64>,
    pub speed: Option<f64>,
    pub filename: String,
    pub tmpfilename: String,
    pub ctx_id: String,
    pub info: Value,
}

impl ProgressData {
    pub fn from_json(info: Value, progress: &Value) -> Self {
        let speed = progress.get("speed");
        Self {
            status: json_str(progress.get("status")),
            downloaded_bytes: nonnegative_int(progress.get("downloaded_bytes")),
            total_bytes: nonnegative_int(progress.get("total_bytes")),
            total_bytes_estimate: nonnegative_int(progress.get("total_bytes_estimate")),
            speed: crate::util::positive_float(speed),
            filename: json_str(progress.get("filename")),
            tmpfilename: json_str(progress.get("tmpfilename")),
            ctx_id: json_str(progress.get("ctx_id")),
            info,
        }
    }
}

#[derive(Clone, Debug)]
struct StreamProgress {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    total_is_estimate: bool,
    speed: f64,
    finished: bool,
}

impl StreamProgress {
    fn new(total_bytes: Option<u64>, total_is_estimate: bool) -> Self {
        Self {
            downloaded_bytes: 0,
            total_bytes,
            total_is_estimate,
            speed: 0.0,
            finished: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransferSnapshot {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub total_is_estimate: bool,
    pub progress: Option<f64>,
    pub speed: f64,
    pub eta: Option<f64>,
    pub all_finished: bool,
    pub should_emit: bool,
}

#[derive(Default)]
struct TrackerState {
    streams: Vec<(String, StreamProgress)>,
    format_keys: HashMap<String, Vec<String>>,
    last_emit_time: Option<Instant>,
}

impl TrackerState {
    fn index_of(&self, key: &str) -> Option<usize> {
        self.streams.iter().position(|(existing, _)| existing == key)
    }

    fn clear(&mut self) {
        self.streams.clear();
        self.format_keys.clear();
        self.last_emit_time = None;
    }

    fn add_stream(&mut self, key: String, format_id: &str, fmt: &Value) {
        let (total, is_estimate) = initial_total(fmt);
        if !format_id.is_empty() {
            self.format_keys.entry(format_id.to_string()).or_default().push(key.clone());
        }
        self.streams.push((key, StreamProgress::new(total, is_estimate)));
    }

    fn find_stream_index(&mut self, data: &ProgressData) -> usize {
        let format_id = json_str(data.info.get("format_id")).trim().to_string();
        if let Some(matching_keys) = self.format_keys.get(&format_id).filter(|keys| !keys.is_empty()) {
            let indices: Vec<usize> = matching_keys.iter().filter_map(|key| self.index_of(key)).collect();
            if let Some(&index) = indices
                .iter()
                .find(|&&index| !self.streams[index].1.finished)
                .or(indices.last())
            {
                return index;
            }
        }

        if self.streams.len() == 1 {
            return 0;
        }

        let filename = Some(data.filename.trim())
            .filter(|name| !name.is_empty())
            .or(Some(data.tmpfilename.trim()).filter(|name| !name.is_empty()))
            .unwrap_or_default()
            .to_string();
        let identity = if !format_id.is_empty() {
            format_id.clone()
        } else if !filename.is_empty() {
            filename
        } else if !data.ctx_id.is_empty() {
            data.ctx_id.clone()
        } else {
            self.streams.len().to_string()
        };
        let key = format!("observed:{identity}");
        if let Some(index) = self.index_of(&key) {
            return index;
        }
        self.add_stream(key, &format_id, &data.info);
        self.streams.len() - 1
    }

    fn snapshot(&self, should_emit: bool) -> TransferSnapshot {
        let streams: Vec<&StreamProgress> = self.streams.iter().map(|(_, stream)| stream).collect();
        let downloaded: u64 = streams.iter().map(|stream| stream.downloaded_bytes).sum();
        let all_totals_known = !streams.is_empty() && streams.iter().all(|stream| stream.total_bytes.is_some());
        let mut total =
            all_totals_known.then(|| streams.iter().map(|stream| stream.total_bytes.unwrap_or(0)).sum::<u64>());
        let speed: f64 = streams.iter().filter(|stream| !stream.finished).map(|stream| stream.speed).sum();
        let all_finished = !streams.is_empty() && streams.iter().all(|stream| stream.finished);

        let (progress, eta) = match total {
            Some(total_bytes) if total_bytes > 0 => {
                let progress = (downloaded as f64 / total_bytes as f64).clamp(0.0, 1.0);
                let eta = (speed > 0.0).then(|| total_bytes.saturating_sub(downloaded) as f64 / speed);
                (Some(progress), eta)
            }
            _ if all_finished => {
                total = (downloaded > 0).then_some(downloaded);
                (Some(1.0), Some(0.0))
            }
            _ => (None, None),
        };

        TransferSnapshot {
            downloaded_bytes: downloaded,
            total_bytes: total,
            total_is_estimate: !all_totals_known || streams.iter().any(|stream| stream.total_is_estimate),
            progress,
            speed,
            eta,
            all_finished,
            should_emit,
        }
    }
}

fn initial_total(fmt: &Value) -> (Option<u64>, bool) {
    if let Some(exact) = nonnegative_int(fmt.get("filesize")).filter(|size| *size > 0) {
        return (Some(exact), false);
    }
    if let Some(estimate) = nonnegative_int(fmt.get("filesize_approx")).filter(|size| *size > 0) {
        return (Some(estimate), true);
    }
    (None, true)
}

#[derive(Default)]
pub struct DownloadProgressTracker {
    state: Mutex<TrackerState>,
}

impl DownloadProgressTracker {
    pub fn reset(&self) {
        self.state.lock().unwrap().clear();
    }

    pub fn prepare(&self, info: &Value) -> TransferSnapshot {
        let formats: Vec<&Value> = match info.get("requested_formats").and_then(Value::as_array) {
            Some(requested) if !requested.is_empty() => requested.iter().filter(|fmt| fmt.is_object()).collect(),
            _ => vec![info],
        };

        let mut state = self.state.lock().unwrap();
        state.clear();

        for (index, fmt) in formats.into_iter().enumerate() {
            let format_id = json_str(fmt.get("format_id")).trim().to_string();
            let base_key = if format_id.is_empty() {
                format!("stream:{index}")
            } else {
                format!("format:{format_id}")
            };
            let mut key = base_key.clone();
            let mut duplicate_index = 2;
            while state.index_of(&key).is_some() {
                key = format!("{base_key}:{duplicate_index}");
                duplicate_index += 1;
            }
            state.add_stream(key, &format_id, fmt);
        }

        state.snapshot(true)
    }

    pub fn update(&self, data: &ProgressData) -> TransferSnapshot {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let index = state.find_stream_index(data);
        let stream = &mut state.streams[index].1;

        let downloaded = data.downloaded_bytes;
        let exact_total = data.total_bytes;

        if let Some(downloaded) = downloaded {
            stream.downloaded_bytes = downloaded;
        }

        if let Some(exact) = exact_total.filter(|total| *total > 0) {
            stream.total_bytes = Some(exact);
            stream.total_is_estimate = false;
        } else if let Some(estimate) = data.total_bytes_estimate.filter(|total| *total > 0) {
            if stream.total_bytes.is_none() || stream.total_is_estimate {
                stream.total_bytes = Some(estimate);
                stream.total_is_estimate = true;
            }
        }

        if stream.total_bytes.is_some_and(|total| stream.downloaded_bytes > total) {
            stream.total_bytes = Some(stream.downloaded_bytes);
        }

        stream.speed = data.speed.unwrap_or(0.0);

        if data.status == "finished" {
            if downloaded.is_none() {
                if let Some(exact) = exact_total {
                    stream.downloaded_bytes = exact;
                } else if let Ok(metadata) = std::fs::metadata(&data.filename) {
                    stream.downloaded_bytes = metadata.len();
                } else if let Some(total) = stream.total_bytes {
                    stream.downloaded_bytes = total;
                }
            }

            if stream.downloaded_bytes > 0 {
                stream.total_bytes = Some(stream.downloaded_bytes);
                stream.total_is_estimate = false;
            }
            stream.speed = 0.0;
            stream.finished = true;
        }

        let force_emit = data.status == "finished" || data.status == "error";
        let should_emit = force_emit
            || state
                .last_emit_time
                .is_none_or(|last| now.duration_since(last) >= PROGRESS_EVENT_INTERVAL);
        if should_emit {
            state.last_emit_time = Some(now);
        }
        state.snapshot(should_emit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregates_video_and_audio_streams() {
        let tracker = DownloadProgressTracker::default();
        let initial = tracker.prepare(&json!({
            "requested_formats": [
                {"format_id": "137", "filesize": 1000},
                {"format_id": "140", "filesize_approx": 200},
            ]
        }));
        assert_eq!(initial.total_bytes, Some(1200));
        assert!(initial.total_is_estimate);
        assert_eq!(initial.progress, Some(0.0));

        let video_done = tracker.update(&ProgressData {
            status: "finished".into(),
            downloaded_bytes: Some(1000),
            total_bytes: Some(1000),
            info: json!({"format_id": "137"}),
            ..Default::default()
        });
        assert!(!video_done.all_finished);
        assert_eq!(video_done.downloaded_bytes, 1000);

        let audio_progress = tracker.update(&ProgressData {
            status: "downloading".into(),
            downloaded_bytes: Some(100),
            total_bytes: Some(250),
            speed: Some(50.0),
            info: json!({"format_id": "140"}),
            ..Default::default()
        });
        assert_eq!(audio_progress.total_bytes, Some(1250));
        assert!(!audio_progress.total_is_estimate);
        assert_eq!(audio_progress.eta, Some(3.0));
    }
}
