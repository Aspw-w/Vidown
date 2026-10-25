use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::util::{APP_NAME, get_local_app_data_directory};

include!(concat!(env!("OUT_DIR"), "/bundled_tools.rs"));

const TOOLS_DIRNAME: &str = "tools";

static TOOLS_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn tools_dir() -> Option<&'static Path> {
    TOOLS_DIR
        .get_or_init(|| {
            if BUNDLED_FILES.is_empty() {
                return None;
            }
            extract().ok()
        })
        .as_deref()
}

pub fn tool(file_name: &str) -> Option<PathBuf> {
    tools_dir().map(|dir| dir.join(file_name)).filter(|path| path.is_file())
}

fn extract() -> std::io::Result<PathBuf> {
    let root = get_local_app_data_directory().join(APP_NAME).join(TOOLS_DIRNAME);
    let dir = root.join(BUNDLE_ID);
    std::fs::create_dir_all(&dir)?;

    for (name, data) in BUNDLED_FILES {
        let path = dir.join(name);
        if path.is_file() {
            continue;
        }
        let temp_path = dir.join(format!("{name}.{}.tmp", std::process::id()));
        let result = std::fs::File::create(&temp_path)
            .and_then(|mut output| zstd::stream::copy_decode(*data, &mut output))
            .and_then(|()| std::fs::rename(&temp_path, &path));
        if let Err(err) = result {
            let _ = std::fs::remove_file(&temp_path);
            if !path.is_file() {
                return Err(err);
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if entry.file_name() != BUNDLE_ID {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    Ok(dir)
}
