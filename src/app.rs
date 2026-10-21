use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Key, Layout, Margin, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, Vec2, ViewportBuilder, ViewportId,
};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageLevel};

use crate::formats::{FormatChoice, describe_selected_format};
use crate::progress::DownloadProgressTracker;
use crate::util::{APP_NAME, detect_platform, expand_user, get_downloads_directory, supported_sites_text};
use crate::worker::{self, DownloadRequest, Emitter, Event, FormatsLoaded};
use crate::ytdlp::YtDlp;

const BROWSER_LABELS: [(&str, Option<&str>); 4] = [
    ("使用しない", None),
    ("Chrome", Some("chrome")),
    ("Edge", Some("edge")),
    ("Firefox", Some("firefox")),
];
const COOKIE_EXPORT_EXTENSION_URL: &str =
    "https://chromewebstore.google.com/detail/cclelndahbckbenkjhflpdbgdldlbecc?utm_source=item-share-cb";

pub const TITLE_FAMILY: &str = "title";
pub const BOLD_FAMILY: &str = "bold";

fn title_font() -> FontId {
    FontId::new(31.0, FontFamily::Name(TITLE_FAMILY.into()))
}

fn subtitle_font() -> FontId {
    FontId::proportional(14.0)
}

fn label_font() -> FontId {
    FontId::new(13.0, FontFamily::Name(BOLD_FAMILY.into()))
}

fn ui_font() -> FontId {
    FontId::proportional(14.0)
}

fn button_font() -> FontId {
    FontId::new(14.0, FontFamily::Name(BOLD_FAMILY.into()))
}

struct Palette {
    window: Color32,
    frame: Color32,
    inner_frame: Color32,
    entry: Color32,
    border: Color32,
    accent: Color32,
    text: Color32,
    muted_text: Color32,
    progress_track: Color32,
}

impl Palette {
    fn for_mode(dark: bool) -> Self {
        if dark {
            Self {
                window: Color32::from_rgb(0x24, 0x24, 0x24),
                frame: Color32::from_rgb(0x2B, 0x2B, 0x2B),
                inner_frame: Color32::from_rgb(0x33, 0x33, 0x33),
                entry: Color32::from_rgb(0x34, 0x36, 0x38),
                border: Color32::from_rgb(0x56, 0x59, 0x5C),
                accent: Color32::from_rgb(0x1F, 0x6A, 0xA5),
                text: Color32::from_gray(0xE5),
                muted_text: Color32::from_gray(0xB3),
                progress_track: Color32::from_rgb(0x4A, 0x4D, 0x50),
            }
        } else {
            Self {
                window: Color32::from_rgb(0xEB, 0xEB, 0xEB),
                frame: Color32::from_rgb(0xDB, 0xDB, 0xDB),
                inner_frame: Color32::from_rgb(0xCF, 0xCF, 0xCF),
                entry: Color32::from_rgb(0xF9, 0xF9, 0xFA),
                border: Color32::from_rgb(0x97, 0x9D, 0xA2),
                accent: Color32::from_rgb(0x3B, 0x8E, 0xD0),
                text: Color32::from_gray(0x1A),
                muted_text: Color32::from_gray(0x59),
                progress_track: Color32::from_rgb(0x93, 0x9B, 0xA2),
            }
        }
    }
}

fn primary_button(palette: &Palette, text: &str) -> Button<'static> {
    Button::new(RichText::new(text).font(button_font()).color(Color32::WHITE))
        .fill(palette.accent)
        .corner_radius(CornerRadius::same(6))
}

fn outline_button(palette: &Palette, text: &str) -> Button<'static> {
    Button::new(RichText::new(text).font(button_font()).color(palette.text))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(6))
}

fn text_entry<'a>(palette: &Palette, text: &'a mut String, hint: &str, height: f32, width: f32) -> TextEdit<'a> {
    TextEdit::singleline(text)
        .hint_text(hint)
        .font(ui_font())
        .background_color(palette.entry)
        .margin(Margin::symmetric(10, ((height - 20.0) / 2.0).max(4.0) as i8))
        .desired_width(width)
}

fn row(ui: &mut Ui, height: f32, add_contents: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), height),
        Layout::right_to_left(Align::Center),
        add_contents,
    );
}

fn show_message(level: MessageLevel, title: &str, description: &str) {
    MessageDialog::new()
        .set_level(level)
        .set_title(title)
        .set_description(description)
        .set_buttons(MessageButtons::Ok)
        .show();
}

struct FormatDialog {
    title: String,
    request: DownloadRequest,
    choices: Vec<FormatChoice>,
    selected: usize,
}

enum DialogAction {
    Confirm,
    Cancel,
}

pub struct VidownApp {
    ytdlp: Arc<YtDlp>,
    url: String,
    folder: String,
    browser_index: usize,
    cookie_file: String,
    status: String,
    is_busy: bool,
    format_dialog: Option<FormatDialog>,
    tracker: Arc<DownloadProgressTracker>,
    progress: Option<f32>,
    events: Receiver<Event>,
    emitter: Emitter,
}

impl VidownApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ytdlp: YtDlp) -> Self {
        let (tx, events) = channel();
        Self {
            ytdlp: Arc::new(ytdlp),
            url: String::new(),
            folder: get_downloads_directory().to_string_lossy().into_owned(),
            browser_index: 0,
            cookie_file: String::new(),
            status: "URLを入力してダウンロードを開始してください。".to_string(),
            is_busy: false,
            format_dialog: None,
            tracker: Arc::new(DownloadProgressTracker::default()),
            progress: Some(0.0),
            events,
            emitter: Emitter::new(tx, cc.egui_ctx.clone()),
        }
    }

    fn browser_name(&self) -> Option<&'static str> {
        BROWSER_LABELS[self.browser_index].1
    }

    fn reset_progress_tracking(&mut self) {
        self.tracker.reset();
        self.progress = Some(0.0);
    }

    fn platform_text(&self) -> String {
        match detect_platform(&self.url) {
            Some(platform) => format!("検出サイト: {platform}"),
            None => format!("対応サイト: {}", supported_sites_text()),
        }
    }

    fn choose_folder(&mut self) {
        let initial_dir = if self.folder.is_empty() {
            get_downloads_directory()
        } else {
            PathBuf::from(&self.folder)
        };
        if let Some(selected) = FileDialog::new().set_directory(initial_dir).pick_folder() {
            self.folder = selected.to_string_lossy().into_owned();
        }
    }

    fn choose_cookie_file(&mut self) {
        let selected = FileDialog::new()
            .set_title("Cookieファイルを選択")
            .add_filter("Cookie files", &["txt", "cookies"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(selected) = selected {
            self.cookie_file = selected.to_string_lossy().into_owned();
        }
    }

    fn open_output_folder(&self) {
        let path = expand_user(&self.folder);
        if path.exists() {
            let _ = open::that_detached(std::fs::canonicalize(&path).unwrap_or(path));
            return;
        }
        show_message(MessageLevel::Warning, "保存先が見つかりません", "指定された保存先フォルダが存在しません。");
    }

    fn start_download(&mut self) {
        if self.is_busy {
            return;
        }

        let url = self.url.trim().to_string();
        let Some(platform) = detect_platform(&url) else {
            show_message(
                MessageLevel::Warning,
                "未対応のURLです",
                "YouTube / Twitter(X) / Instagram のURLを入力してください。",
            );
            return;
        };

        let output_dir = expand_user(&self.folder);
        if let Err(err) = std::fs::create_dir_all(&output_dir) {
            show_message(MessageLevel::Error, "保存先を作成できません", &err.to_string());
            return;
        }

        self.format_dialog = None;

        let browser_name = self.browser_name();
        let mut cookie_file_path = None;
        if browser_name.is_some() {
            let cookie_file_text = self.cookie_file.trim();
            if cookie_file_text.is_empty() {
                show_message(
                    MessageLevel::Warning,
                    "Cookieファイルが必要です",
                    "ブラウザCookieを使用するにはCookieファイルを指定してください。",
                );
                return;
            }

            let path = expand_user(cookie_file_text);
            if !path.is_file() {
                show_message(
                    MessageLevel::Warning,
                    "Cookieファイルが見つかりません",
                    "指定されたCookieファイルが存在しません。",
                );
                return;
            }
            cookie_file_path = Some(path);
        }

        self.is_busy = true;
        self.reset_progress_tracking();
        self.status = format!("{platform} の利用可能な画質を取得しています...");
        self.append_log("");
        self.append_log(&format!("URL: {url}"));
        self.append_log(&format!("保存先: {}", output_dir.display()));
        self.append_log(&format!("サイト: {platform}"));
        if let Some(path) = &cookie_file_path {
            self.append_log(&format!("Cookieファイル: {}", path.display()));
        }

        let request = DownloadRequest {
            url,
            output_dir,
            browser_name,
            cookie_file_path,
            platform,
        };
        let ytdlp = Arc::clone(&self.ytdlp);
        let emitter = self.emitter.clone();
        std::thread::spawn(move || worker::fetch_available_formats(&ytdlp, &emitter, request));
    }

    fn show_format_dialog(&mut self, loaded: FormatsLoaded) {
        self.status = "利用可能な画質を選択してください。".to_string();
        self.append_log(&format!("タイトル: {}", loaded.title));
        self.append_log(&format!("取得形式数: {}", loaded.choices.len()));
        self.format_dialog = Some(FormatDialog {
            title: loaded.title,
            request: loaded.request,
            choices: loaded.choices,
            selected: 0,
        });
    }

    fn confirm_format(&mut self, dialog: FormatDialog) {
        let Some(choice) = dialog.choices.into_iter().nth(dialog.selected) else {
            return;
        };
        self.reset_progress_tracking();
        self.append_log(&format!("選択形式: {}", describe_selected_format(&choice)));
        self.status = format!("{} をダウンロード中...", dialog.title);

        let ytdlp = Arc::clone(&self.ytdlp);
        let emitter = self.emitter.clone();
        let tracker = Arc::clone(&self.tracker);
        let request = dialog.request;
        std::thread::spawn(move || worker::download_selected_format(&ytdlp, &emitter, &tracker, request, choice));
    }

    fn cancel_format(&mut self) {
        self.is_busy = false;
        self.reset_progress_tracking();
        self.status = "形式選択をキャンセルしました。".to_string();
        self.append_log("形式選択をキャンセルしました。");
    }

    fn process_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Log(message) => self.append_log(&message),
                Event::Progress { progress, message } => {
                    self.progress = progress.map(|value| value.clamp(0.0, 1.0) as f32);
                    self.status = message;
                }
                Event::FormatsLoaded(loaded) => self.show_format_dialog(loaded),
                Event::Done { success, message } => {
                    self.is_busy = false;
                    self.status = message.clone();
                    if success {
                        self.progress = Some(1.0);
                        self.append_log(&message);
                        show_message(MessageLevel::Info, "完了", &message);
                    } else {
                        self.reset_progress_tracking();
                        self.append_log(&format!("失敗: {message}"));
                        show_message(MessageLevel::Error, "ダウンロードに失敗しました", &message);
                    }
                }
            }
        }
    }

    fn append_log(&self, _message: &str) {}

    fn progress_bar(&self, ui: &mut Ui, palette: &Palette) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 8.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let radius = CornerRadius::same(4);
        painter.rect_filled(rect, radius, palette.progress_track);

        match self.progress {
            Some(value) => {
                if value > 0.0 {
                    let mut filled = rect;
                    filled.set_width(rect.width() * value);
                    painter.rect_filled(filled, radius, palette.accent);
                }
            }
            None => {
                let time = ui.input(|input| input.time) as f32;
                let segment = rect.width() * 0.25;
                let phase = (time * 0.6).fract();
                let start = rect.left() - segment + (rect.width() + segment) * phase;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(start.max(rect.left()), rect.top()),
                    egui::pos2((start + segment).min(rect.right()), rect.bottom()),
                );
                painter.rect_filled(bar, radius, palette.accent);
                ui.ctx().request_repaint();
            }
        }
    }

    fn main_ui(&mut self, ui: &mut Ui, palette: &Palette) {
        Frame::new()
            .fill(palette.frame)
            .corner_radius(CornerRadius::same(20))
            .inner_margin(Margin::same(24))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(10.0, 0.0);
                ui.label(RichText::new(APP_NAME).font(title_font()).color(palette.text));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("開始後に利用可能な画質一覧を取得し、選んだ形式で動画を保存します。")
                        .font(subtitle_font())
                        .color(palette.muted_text),
                );
                ui.add_space(18.0);

                ui.label(RichText::new("動画URL").font(label_font()).color(palette.text));
                ui.add_space(8.0);
                let url_response = ui.add(text_entry(
                    palette,
                    &mut self.url,
                    "https://www.youtube.com/watch?v=... / https://x.com/... / https://www.instagram.com/...",
                    42.0,
                    ui.available_width(),
                ));
                if url_response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                    self.start_download();
                }
                ui.add_space(6.0);
                ui.label(RichText::new(self.platform_text()).font(subtitle_font()).color(palette.muted_text));
                ui.add_space(18.0);

                Frame::new()
                    .fill(palette.inner_frame)
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| self.settings_ui(ui, palette));
                ui.add_space(16.0);

                row(ui, 44.0, |ui| {
                    if ui.add(outline_button(palette, "保存先を開く").min_size(Vec2::new(150.0, 44.0))).clicked() {
                        self.open_output_folder();
                    }
                    ui.add_space(2.0);
                    let download = primary_button(palette, "ダウンロード開始")
                        .min_size(Vec2::new(ui.available_width(), 44.0));
                    if ui.add_enabled(!self.is_busy, download).clicked() {
                        self.start_download();
                    }
                });
                ui.add_space(18.0);

                self.progress_bar(ui, palette);
                ui.add_space(8.0);
                ui.label(RichText::new(&self.status).font(ui_font()).color(palette.text));
            });
    }

    fn settings_ui(&mut self, ui: &mut Ui, palette: &Palette) {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("保存先フォルダ").font(label_font()).color(palette.text));
        ui.add_space(6.0);
        row(ui, 40.0, |ui| {
            if ui.add(primary_button(palette, "参照").min_size(Vec2::new(100.0, 40.0))).clicked() {
                self.choose_folder();
            }
            ui.add(text_entry(palette, &mut self.folder, "", 40.0, ui.available_width()));
        });
        ui.add_space(14.0);

        ui.label(RichText::new("ブラウザCookie").font(label_font()).color(palette.text));
        ui.add_space(6.0);
        let previous_width = ui.spacing().combo_width;
        let previous_height = ui.spacing().interact_size.y;
        ui.spacing_mut().combo_width = ui.available_width();
        ui.spacing_mut().interact_size.y = 40.0;
        egui::ComboBox::from_id_salt("browser_cookie")
            .selected_text(RichText::new(BROWSER_LABELS[self.browser_index].0).font(ui_font()))
            .height(200.0)
            .show_ui(ui, |ui| {
                for (index, (label, _)) in BROWSER_LABELS.iter().enumerate() {
                    ui.selectable_value(&mut self.browser_index, index, RichText::new(*label).font(ui_font()));
                }
            });
        ui.spacing_mut().combo_width = previous_width;
        ui.spacing_mut().interact_size.y = previous_height;
        ui.add_space(14.0);

        if self.browser_name().is_some() {
            ui.label(RichText::new("Cookieファイル（必須）").font(label_font()).color(palette.text));
            ui.add_space(6.0);
            row(ui, 40.0, |ui| {
                if ui
                    .add(primary_button(palette, "Cookie出力拡張機能を開く").min_size(Vec2::new(190.0, 40.0)))
                    .clicked()
                {
                    let _ = open::that_detached(COOKIE_EXPORT_EXTENSION_URL);
                }
                if ui.add(outline_button(palette, "解除").min_size(Vec2::new(72.0, 40.0))).clicked() {
                    self.cookie_file.clear();
                }
                if ui.add(primary_button(palette, "参照").min_size(Vec2::new(86.0, 40.0))).clicked() {
                    self.choose_cookie_file();
                }
                ui.add(text_entry(
                    palette,
                    &mut self.cookie_file,
                    "Netscape形式の cookies.txt を指定してください",
                    40.0,
                    ui.available_width(),
                ));
            });
            ui.add_space(14.0);
        }
    }

    fn format_dialog_ui(&mut self, ctx: &egui::Context, palette: &Palette) -> Option<DialogAction> {
        let dialog = self.format_dialog.as_mut()?;
        let mut action = None;
        let viewport = ViewportBuilder::default()
            .with_title("画質を選択")
            .with_inner_size([780.0, 560.0])
            .with_min_inner_size([700.0, 500.0]);

        ctx.show_viewport_immediate(ViewportId::from_hash_of("format_dialog"), viewport, |ui, _class| {
            if ui.input(|input| input.viewport().close_requested()) {
                action = Some(DialogAction::Cancel);
            }

            egui::Panel::bottom("format_dialog_buttons")
                .frame(Frame::new().fill(palette.window).inner_margin(Margin {
                    left: 20,
                    right: 20,
                    top: 0,
                    bottom: 20,
                }))
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(outline_button(palette, "キャンセル").min_size(Vec2::new(140.0, 32.0))).clicked() {
                            action = Some(DialogAction::Cancel);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(primary_button(palette, "この形式で保存").min_size(Vec2::new(170.0, 32.0))).clicked()
                            {
                                action = Some(DialogAction::Confirm);
                            }
                        });
                    });
                });

            egui::CentralPanel::default()
                .frame(Frame::new().fill(palette.window).inner_margin(Margin::same(20)))
                .show(ui, |ui| {
                    Frame::new()
                        .fill(palette.frame)
                        .corner_radius(CornerRadius::same(18))
                        .inner_margin(Margin::same(18))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                RichText::new("利用可能な画質を選択してください").font(title_font()).color(palette.text),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!("{} / {}", dialog.request.platform, dialog.title))
                                    .font(subtitle_font())
                                    .color(palette.muted_text),
                            );
                        });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(
                            "音声入りの形式を優先して並べています。映像と音声を結合する形式は ffmpeg を使います。",
                        )
                        .font(subtitle_font())
                        .color(palette.muted_text),
                    );
                    ui.add_space(12.0);

                    Frame::new()
                        .fill(palette.frame)
                        .corner_radius(CornerRadius::same(16))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                                for (index, choice) in dialog.choices.iter().enumerate() {
                                    ui.radio_value(
                                        &mut dialog.selected,
                                        index,
                                        RichText::new(&choice.title).font(ui_font()).color(palette.text),
                                    );
                                    ui.horizontal(|ui| {
                                        ui.add_space(30.0);
                                        ui.label(
                                            RichText::new(&choice.details)
                                                .font(subtitle_font())
                                                .color(palette.muted_text),
                                        );
                                    });
                                    ui.add_space(12.0);
                                }
                            });
                        });
                });
        });
        action
    }
}

impl eframe::App for VidownApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.process_events();

        let ctx = ui.ctx().clone();
        let palette = Palette::for_mode(ui.visuals().dark_mode);

        if let Some(action) = self.format_dialog_ui(&ctx, &palette) {
            if let Some(dialog) = self.format_dialog.take() {
                match action {
                    DialogAction::Confirm => self.confirm_format(dialog),
                    DialogAction::Cancel => self.cancel_format(),
                }
            }
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(palette.window))
            .show(ui, |ui| {
                let viewport_width = ui.available_width();
                ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    Frame::new().inner_margin(Margin::same(24)).show(ui, |ui| {
                        ui.set_width((viewport_width - 48.0).max(660.0));
                        ui.add_enabled_ui(self.format_dialog.is_none(), |ui| self.main_ui(ui, &palette));
                    });
                });
            });
    }
}
