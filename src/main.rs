// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod lang;
mod log;
mod monitor;
mod sys;
mod theme;
mod ui;
mod usbipd;

use eframe::egui;

include!(concat!(env!("OUT_DIR"), "/app_version.rs"));

const MUTEX_NAME: &str = "usbip-device-manager-3f27c19a-2e11-4fc4-9b18-6d3f5d9a6b21";

fn main() -> eframe::Result<()> {
    log::init();

    if sys::acquire_single_instance(MUTEX_NAME).is_none() {
        let zh = sys::system_locale_is_chinese();
        sys::message_box(
            if zh { "提示" } else { "Note" },
            if zh {
                "程序已在运行，请在系统托盘图标处恢复窗口。"
            } else {
                "Another instance of the app is already running,\r\ncheck the system tray icon to restore the instance."
            },
        );
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("USBIP Device Manager")
        .with_inner_size([920.0, 640.0])
        .with_min_inner_size([760.0, 480.0]);
    // 标题栏图标与托盘使用同一张图标。
    if let Some(icon) = app::load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "usbip-device-manager",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc)?))
        }),
    )
}

/// 内置 Noto Sans CJK SC，保证中英文字形一致、不依赖系统字体。
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/NotoSansCJKsc-Regular.otf"
        ))),
    );
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        list.insert(0, "cjk".to_owned());
    }
    if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        list.push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}
