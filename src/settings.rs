use std::path::PathBuf;

use serde_json::{Value, json};

use crate::util::{APP_NAME, get_roaming_app_data_directory};

const SETTINGS_FILENAME: &str = "settings.json";
const OUTPUT_DIR_KEY: &str = "output_dir";

pub struct Settings {
    pub output_dir: Option<String>,
}

fn settings_directory() -> PathBuf {
    get_roaming_app_data_directory().join(APP_NAME)
}

fn settings_path() -> PathBuf {
    settings_directory().join(SETTINGS_FILENAME)
}

impl Settings {
    pub fn load() -> Self {
        let _ = std::fs::create_dir_all(settings_directory());
        let output_dir = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.get(OUTPUT_DIR_KEY)?.as_str().map(str::to_string))
            .filter(|output_dir| !output_dir.trim().is_empty());
        Self { output_dir }
    }

    pub fn save_output_dir(output_dir: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(settings_directory())?;
        let data = serde_json::to_string_pretty(&json!({ OUTPUT_DIR_KEY: output_dir }))?;
        std::fs::write(settings_path(), data)
    }
}
