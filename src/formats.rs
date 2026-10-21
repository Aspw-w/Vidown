use std::cmp::Ordering;
use std::collections::HashSet;

use serde_json::Value;

use crate::util::{display_file_size, json_f64, json_str, json_truthy};

#[derive(Clone, Debug)]
pub struct FormatChoice {
    pub selector: String,
    pub title: String,
    pub details: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub format_id: String,
    pub is_muxed: bool,
    pub height: i64,
    sort_key: (i64, i32, i32, f64, f64),
}

impl FormatChoice {
    fn cmp_sort_key(&self, other: &Self) -> Ordering {
        let (a, b) = (&self.sort_key, &other.sort_key);
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.total_cmp(&b.3))
            .then(a.4.total_cmp(&b.4))
    }
}

/// `value in {None, "none"}` for a codec field.
fn is_none_codec(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null)) || value.and_then(Value::as_str) == Some("none")
}

fn codec_label(codec: Option<&Value>) -> Option<String> {
    if is_none_codec(codec) {
        return None;
    }
    let text = match codec? {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    Some(text.split('.').next().unwrap_or_default().to_string())
}

fn is_video_format(fmt: &Value) -> bool {
    if fmt.get("vcodec").and_then(Value::as_str) == Some("none") {
        return false;
    }
    json_truthy(fmt.get("height")) || json_truthy(fmt.get("width")) || !is_none_codec(fmt.get("vcodec"))
}

fn is_social_platform(platform: &str) -> bool {
    platform == "Twitter/X" || platform == "Instagram"
}

fn looks_like_muxed_format(fmt: &Value, platform: &str) -> bool {
    if !is_none_codec(fmt.get("acodec")) {
        return true;
    }

    let protocol = json_str(fmt.get("protocol"));
    let format_id = json_str(fmt.get("format_id"));
    is_social_platform(platform) && (protocol == "https" || format_id.starts_with("http-"))
}

/// Returns `(selector, is_muxed)`; non-muxed selectors require an ffmpeg merge.
fn build_selector_for_format(fmt: &Value, platform: &str, ffmpeg_available: bool) -> Option<(String, bool)> {
    let format_id = json_str(fmt.get("format_id")).trim().to_string();
    if format_id.is_empty() || !is_video_format(fmt) {
        return None;
    }

    if looks_like_muxed_format(fmt, platform) {
        return Some((format_id, true));
    }

    if !ffmpeg_available {
        return None;
    }

    let selector = format!(
        "{format_id}+bestaudio[ext=m4a][acodec!=none]/{format_id}+bestaudio[acodec!=none]/{format_id}+bestaudio"
    );
    Some((selector, false))
}

fn build_format_choice(fmt: &Value, platform: &str, ffmpeg_available: bool) -> Option<FormatChoice> {
    let (selector, is_muxed) = build_selector_for_format(fmt, platform, ffmpeg_available)?;

    let format_id = json_str(fmt.get("format_id"));
    let ext = Some(json_str(fmt.get("ext")))
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let height = json_f64(fmt.get("height")).trunc() as i64;
    let fps = json_f64(fmt.get("fps"));
    let tbr = json_f64(fmt.get("tbr"));
    let size = [fmt.get("filesize"), fmt.get("filesize_approx")]
        .into_iter()
        .find(|value| json_truthy(*value))
        .map(json_f64);
    let protocol = Some(json_str(fmt.get("protocol")))
        .filter(|protocol| !protocol.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
        .replace("_native", "")
        .to_uppercase();
    let resolution = if height != 0 {
        format!("{height}p")
    } else {
        Some(json_str(fmt.get("resolution")))
            .filter(|resolution| !resolution.is_empty())
            .unwrap_or_else(|| "不明".to_string())
    };
    let fps_label = (fps >= 1.0).then(|| format!("{}fps", fps.round() as i64));
    let note = Some(json_str(fmt.get("format_note")).trim().to_string()).filter(|note| !note.is_empty());
    let vcodec = codec_label(fmt.get("vcodec"));
    let audio_state = if is_muxed { "音声入り" } else { "映像と音声を結合" };

    let title_parts = [resolution, ext.to_uppercase(), audio_state.to_string(), format!("id={format_id}")];
    let mut detail_parts = vec![protocol, display_file_size(size)];
    detail_parts.extend(fps_label);
    detail_parts.extend(note);
    detail_parts.extend(vcodec);

    Some(FormatChoice {
        selector,
        title: title_parts.join(" | "),
        details: detail_parts.join(" / "),
        format_id,
        is_muxed,
        height,
        sort_key: (
            height,
            i32::from(is_muxed),
            i32::from(ext.eq_ignore_ascii_case("mp4")),
            fps,
            tbr,
        ),
    })
}

pub fn build_format_choices(info: &Value, platform: &str, ffmpeg_available: bool) -> Vec<FormatChoice> {
    let Some(raw_formats) = info.get("formats").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut choices: Vec<FormatChoice> = Vec::new();
    let mut seen_selectors: HashSet<String> = HashSet::new();
    let mut muxed_keys: HashSet<(i64, String)> = HashSet::new();

    for fmt in raw_formats {
        if !fmt.is_object() {
            continue;
        }
        let Some(choice) = build_format_choice(fmt, platform, ffmpeg_available) else {
            continue;
        };
        if !choice.is_muxed || seen_selectors.contains(&choice.selector) {
            continue;
        }
        seen_selectors.insert(choice.selector.clone());
        muxed_keys.insert((choice.height, json_str(fmt.get("ext"))));
        choices.push(choice);
    }

    for fmt in raw_formats {
        if !fmt.is_object() {
            continue;
        }
        let Some(choice) = build_format_choice(fmt, platform, ffmpeg_available) else {
            continue;
        };
        if choice.is_muxed || seen_selectors.contains(&choice.selector) {
            continue;
        }
        if is_social_platform(platform) && muxed_keys.contains(&(choice.height, json_str(fmt.get("ext")))) {
            continue;
        }
        seen_selectors.insert(choice.selector.clone());
        choices.push(choice);
    }

    // Stable sort, descending, like Python's `sort(reverse=True)`.
    choices.sort_by(|a, b| b.cmp_sort_key(a));
    choices
}

pub fn describe_selected_format(choice: &FormatChoice) -> String {
    format!("{} ({})", choice.title, choice.details)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn orders_muxed_and_merged_formats() {
        let info = json!({
            "formats": [
                {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2"},
                {"format_id": "18", "ext": "mp4", "height": 360, "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "protocol": "https", "filesize": 1048576},
                {"format_id": "137", "ext": "mp4", "height": 1080, "fps": 30, "vcodec": "avc1.640028", "acodec": "none", "protocol": "https"},
            ]
        });

        let choices = build_format_choices(&info, "YouTube", true);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].format_id, "137");
        assert!(!choices[0].is_muxed);
        assert_eq!(choices[0].title, "1080p | MP4 | 映像と音声を結合 | id=137");
        assert_eq!(choices[0].details, "HTTPS / サイズ不明 / 30fps / avc1");
        assert_eq!(choices[1].title, "360p | MP4 | 音声入り | id=18");
        assert_eq!(choices[1].details, "HTTPS / 1.0 MB / avc1");

        let without_ffmpeg = build_format_choices(&info, "YouTube", false);
        assert_eq!(without_ffmpeg.len(), 1);
        assert_eq!(without_ffmpeg[0].selector, "18");
    }

    #[test]
    fn social_platforms_treat_https_as_muxed() {
        let info = json!({
            "formats": [
                {"format_id": "http-832", "ext": "mp4", "height": 720, "protocol": "https"},
                {"format_id": "hls-832", "ext": "mp4", "height": 720, "protocol": "m3u8_native", "vcodec": "avc1", "acodec": "none"},
            ]
        });
        let choices = build_format_choices(&info, "Twitter/X", true);
        assert_eq!(choices.len(), 1);
        assert!(choices[0].is_muxed);
    }
}
