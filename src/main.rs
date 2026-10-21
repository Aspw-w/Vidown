#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cookies;
mod formats;
mod parallel_http;
mod progress;
mod util;
mod worker;
mod ytdlp;

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use rfd::{MessageButtons, MessageDialog, MessageLevel};

use crate::app::{BOLD_FAMILY, TITLE_FAMILY, VidownApp};
use crate::util::{APP_ID, APP_NAME, find_ytdlp, set_windows_app_id};

const APP_ICON: &[u8] = include_bytes!("../icon.ico");

/// `(file, face index)` candidates in the Windows font folder, in order of preference.
const BODY_FONTS: [(&str, u32); 3] = [("YuGothM.ttc", 1), ("segoeui.ttf", 0), ("meiryo.ttc", 2)];
const BOLD_FONTS: [(&str, u32); 3] = [("YuGothB.ttc", 1), ("segoeuib.ttf", 0), ("meiryob.ttc", 2)];
const TITLE_FONTS: [(&str, u32); 2] = [("segoeuib.ttf", 0), ("YuGothB.ttc", 1)];

fn show_dependency_error(package_name: &str) {
    MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title("依存パッケージが見つかりません")
        .set_description(format!(
            "{package_name} がインストールされていません。\n\n\
             {package_name}.exe を {APP_NAME}.exe と同じフォルダに置くか、PATH に追加してください。"
        ))
        .set_buttons(MessageButtons::Ok)
        .show();
}

fn load_window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory_with_format(APP_ICON, image::ImageFormat::Ico)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn install_fonts(ctx: &egui::Context) {
    let fonts_dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");

    let mut fonts = FontDefinitions::default();
    let default_proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut register = |candidates: &[(&str, u32)]| -> Vec<String> {
        candidates
            .iter()
            .filter_map(|&(file, index)| {
                let name = format!("{file}#{index}");
                if !fonts.font_data.contains_key(&name) {
                    let bytes = std::fs::read(fonts_dir.join(file)).ok()?;
                    let mut data = FontData::from_owned(bytes);
                    data.index = index;
                    fonts.font_data.insert(name.clone(), Arc::new(data));
                }
                Some(name)
            })
            .collect()
    };

    let body = register(&BODY_FONTS);
    let bold = register(&BOLD_FONTS);
    let title = register(&TITLE_FONTS);

    let with_fallbacks = |primary: Vec<String>, fallbacks: &[String]| -> Vec<String> {
        let mut family = primary;
        family.extend(fallbacks.iter().filter(|name| !family.contains(name)).cloned().collect::<Vec<_>>());
        family.extend(default_proportional.iter().cloned());
        family
    };

    fonts
        .families
        .insert(FontFamily::Proportional, with_fallbacks(body.clone(), &[]));
    fonts
        .families
        .insert(FontFamily::Name(BOLD_FAMILY.into()), with_fallbacks(bold.clone(), &body));
    fonts
        .families
        .insert(FontFamily::Name(TITLE_FAMILY.into()), with_fallbacks(title, &[bold, body].concat()));
    let proportional = fonts.families[&FontFamily::Proportional].clone();
    if let Some(monospace) = fonts.families.get_mut(&FontFamily::Monospace) {
        monospace.extend(proportional);
    }

    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    set_windows_app_id(APP_ID);

    let Some(ytdlp_path) = find_ytdlp() else {
        show_dependency_error("yt-dlp");
        std::process::exit(1);
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id(APP_ID)
        .with_inner_size([880.0, 740.0])
        .with_min_inner_size([520.0, 420.0]);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::ThemePreference::System);
            Ok(Box::new(VidownApp::new(cc, ytdlp::YtDlp::new(ytdlp_path))))
        }),
    )
}
