use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::bundled;

pub const APP_NAME: &str = "Vidown";
pub const APP_ID: &str = "Vidown";

pub const SUPPORTED_PLATFORMS: [&str; 3] = ["YouTube", "Twitter/X", "Instagram"];

pub fn supported_sites_text() -> String {
    SUPPORTED_PLATFORMS.join(" / ")
}

pub fn nonnegative_int(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => {
            if let Some(result) = number.as_u64() {
                Some(result)
            } else if number.as_i64().is_some() {
                None
            } else {
                let result = number.as_f64()?.trunc();
                (result.is_finite() && result >= 0.0 && result < u64::MAX as f64).then_some(result as u64)
            }
        }
        Value::String(text) => text.trim().parse::<u64>().ok(),
        Value::Bool(flag) => Some(u64::from(*flag)),
        _ => None,
    }
}

pub fn positive_float(value: Option<&Value>) -> Option<f64> {
    let result = match value? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (result.is_finite() && result > 0.0).then_some(result)
}

pub fn json_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => String::new(),
        Some(other) => other.to_string(),
    }
}

pub fn json_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

pub fn json_f64(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn split_scheme_and_netloc(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some((scheme.to_ascii_lowercase(), rest[..netloc_end].to_string()))
}

pub fn is_http_url(url: &str) -> bool {
    split_scheme_and_netloc(url).is_some_and(|(scheme, _)| scheme == "http" || scheme == "https")
}

pub fn detect_platform(url: &str) -> Option<&'static str> {
    let (scheme, netloc) = split_scheme_and_netloc(url.trim())?;
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let lowered = netloc.to_lowercase();
    let host = lowered.strip_prefix("www.").unwrap_or(&lowered);

    if host == "youtu.be" || host.ends_with("youtube.com") {
        return Some("YouTube");
    }
    if host.ends_with("twitter.com") || host.ends_with("x.com") {
        return Some("Twitter/X");
    }
    if host.ends_with("instagram.com") {
        return Some("Instagram");
    }
    None
}

pub fn human_readable_size(num_bytes: Option<f64>) -> String {
    let Some(mut size) = num_bytes.filter(|value| *value != 0.0) else {
        return "0 B".to_string();
    };

    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    for (index, unit) in UNITS.iter().enumerate() {
        if size < 1024.0 || index == UNITS.len() - 1 {
            return format!("{size:.1} {unit}");
        }
        size /= 1024.0;
    }
    format!("{size:.1} TB")
}

pub fn display_file_size(num_bytes: Option<f64>) -> String {
    match num_bytes {
        Some(value) if value != 0.0 => human_readable_size(Some(value)),
        _ => "サイズ不明".to_string(),
    }
}

pub fn format_eta(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "--:--".to_string();
    };

    let total_seconds = if seconds.is_finite() { seconds.max(0.0) as u64 } else { 0 };
    let (minutes, remaining_seconds) = (total_seconds / 60, total_seconds % 60);
    let (hours, remaining_minutes) = (minutes / 60, minutes % 60);

    if hours > 0 {
        format!("{hours:02}:{remaining_minutes:02}:{remaining_seconds:02}")
    } else {
        format!("{remaining_minutes:02}:{remaining_seconds:02}")
    }
}

pub fn detect_js_runtime() -> Option<(&'static str, PathBuf)> {
    const CANDIDATES: [(&str, &[&str]); 4] = [
        ("node", &["node"]),
        ("deno", &["deno"]),
        ("quickjs", &["qjs", "quickjs"]),
        ("bun", &["bun"]),
    ];

    if let Some(path) = bundled::tool("deno.exe") {
        return Some(("deno", path));
    }

    CANDIDATES.iter().find_map(|(runtime_name, commands)| {
        commands
            .iter()
            .find_map(|command| which::which(command).ok())
            .map(|path| (*runtime_name, path))
    })
}

pub fn find_ffmpeg() -> Option<PathBuf> {
    bundled::tool("ffmpeg.exe").or_else(|| which::which("ffmpeg").ok())
}

pub fn find_ytdlp() -> Option<PathBuf> {
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        for name in ["yt-dlp.exe", "yt-dlp"] {
            let candidate = exe_dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    bundled::tool("yt-dlp.exe").or_else(|| which::which("yt-dlp").ok())
}

pub fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        if let Some(home) = home_dir() {
            return home.join(path[1..].trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(windows)]
fn get_windows_known_folder(folder_id: &windows_sys::core::GUID) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;

    let mut path_ptr: *mut u16 = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(folder_id, 0, std::ptr::null_mut(), &mut path_ptr) };
    if result != 0 || path_ptr.is_null() {
        if !path_ptr.is_null() {
            unsafe { CoTaskMemFree(path_ptr.cast()) };
        }
        return None;
    }

    let path = unsafe {
        let len = (0..).take_while(|&i| *path_ptr.add(i) != 0).count();
        std::ffi::OsString::from_wide(std::slice::from_raw_parts(path_ptr, len))
    };
    unsafe { CoTaskMemFree(path_ptr.cast()) };
    Some(PathBuf::from(path)).filter(|path| !path.as_os_str().is_empty())
}

pub fn get_downloads_directory() -> PathBuf {
    #[cfg(windows)]
    if let Some(downloads_dir) = get_windows_known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Downloads) {
        return downloads_dir;
    }
    home_dir().unwrap_or_default().join("Downloads")
}

pub fn get_roaming_app_data_directory() -> PathBuf {
    #[cfg(windows)]
    if let Some(app_data_dir) = get_windows_known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_RoamingAppData) {
        return app_data_dir;
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().unwrap_or_default().join("AppData").join("Roaming"))
}

pub fn get_local_app_data_directory() -> PathBuf {
    #[cfg(windows)]
    if let Some(app_data_dir) = get_windows_known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_LocalAppData) {
        return app_data_dir;
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().unwrap_or_default().join("AppData").join("Local"))
}

pub fn set_windows_app_id(app_id: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let wide: Vec<u16> = app_id.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    }
    #[cfg(not(windows))]
    let _ = app_id;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_platforms() {
        assert_eq!(detect_platform("https://www.youtube.com/watch?v=x"), Some("YouTube"));
        assert_eq!(detect_platform("https://youtu.be/x"), Some("YouTube"));
        assert_eq!(detect_platform("https://x.com/a/status/1"), Some("Twitter/X"));
        assert_eq!(detect_platform("HTTPS://Twitter.com/a"), Some("Twitter/X"));
        assert_eq!(detect_platform("https://www.instagram.com/p/x/"), Some("Instagram"));
        assert_eq!(detect_platform("ftp://youtube.com/"), None);
        assert_eq!(detect_platform("https://example.com/"), None);
        assert_eq!(detect_platform("youtube.com"), None);
    }

    #[test]
    fn formats_sizes_and_eta() {
        assert_eq!(human_readable_size(None), "0 B");
        assert_eq!(human_readable_size(Some(512.0)), "512.0 B");
        assert_eq!(human_readable_size(Some(1536.0)), "1.5 KB");
        assert_eq!(display_file_size(Some(0.0)), "サイズ不明");
        assert_eq!(format_eta(None), "--:--");
        assert_eq!(format_eta(Some(65.9)), "01:05");
        assert_eq!(format_eta(Some(3725.0)), "01:02:05");
    }
}
