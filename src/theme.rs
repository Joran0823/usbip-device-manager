// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! 主题感知颜色与全局 Style。
//! 深色/亮色使用同一套色调体系，所有控件从 Visuals/Style 取色，
//! 避免 egui 默认主题发灰、发糊的问题。

#![allow(dead_code)]

use eframe::egui;
use std::cell::Cell;

// ---- 深色主题色 ----
const BG_D: egui::Color32 = egui::Color32::from_rgb(0x13, 0x17, 0x1D);
const PANEL_D: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x20, 0x28);
const STATUS_BG_D: egui::Color32 = egui::Color32::from_rgb(0x16, 0x1A, 0x21);
const INPUT_BG_D: egui::Color32 = egui::Color32::from_rgb(0x10, 0x14, 0x1A);
const BTN_GRAY_D: egui::Color32 = egui::Color32::from_rgb(0x25, 0x2B, 0x35);
const TEXT_D: egui::Color32 = egui::Color32::from_rgb(0xE5, 0xE8, 0xEC);
const TEXT_SOFT_D: egui::Color32 = egui::Color32::from_rgb(0x99, 0xA1, 0xAD);
const BORDER_COLOR_D: egui::Color32 = egui::Color32::from_rgb(0x3A, 0x41, 0x4B);

// ---- 亮色主题色 ----
const BG_L: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xF3, 0xF5);
const PANEL_L: egui::Color32 = egui::Color32::from_rgb(0xE9, 0xEB, 0xEF);
const STATUS_BG_L: egui::Color32 = egui::Color32::from_rgb(0xE2, 0xE4, 0xE9);
const INPUT_BG_L: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xFF, 0xFF);
const BTN_GRAY_L: egui::Color32 = egui::Color32::from_rgb(0xE2, 0xE4, 0xE9);
const TEXT_L: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x24, 0x2B);
const TEXT_SOFT_L: egui::Color32 = egui::Color32::from_rgb(0x6B, 0x73, 0x80);
const BORDER_COLOR_L: egui::Color32 = egui::Color32::from_rgb(0xC2, 0xC8, 0xD1);

// ---- 主题无关强调色 ----
pub const BLUE: egui::Color32 = egui::Color32::from_rgb(0x45, 0x96, 0xFF);
pub const BLUE_CHECK: egui::Color32 = egui::Color32::from_rgb(0x2E, 0x7C, 0xF6);
pub const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(0xF0, 0x6A, 0x6A);
pub const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(0x4C, 0xC3, 0x6A);
pub const CORNER: u8 = 6;

// ---- 主题状态（egui UI 只在单线程渲染，用线程局部变量即可） ----
thread_local! {
    static IS_DARK: Cell<bool> = const { Cell::new(true) };
}

pub fn is_dark() -> bool {
    IS_DARK.with(|dark| dark.get())
}

fn set_dark(is_dark: bool) {
    IS_DARK.with(|dark| dark.set(is_dark));
}

// ---- 主题感知 getter ----
pub fn bg() -> egui::Color32 {
    if is_dark() { BG_D } else { BG_L }
}
pub fn panel() -> egui::Color32 {
    if is_dark() { PANEL_D } else { PANEL_L }
}
pub fn input_bg() -> egui::Color32 {
    if is_dark() { INPUT_BG_D } else { INPUT_BG_L }
}
pub fn btn_gray() -> egui::Color32 {
    if is_dark() { BTN_GRAY_D } else { BTN_GRAY_L }
}
pub fn text() -> egui::Color32 {
    if is_dark() { TEXT_D } else { TEXT_L }
}
pub fn text_soft() -> egui::Color32 {
    if is_dark() { TEXT_SOFT_D } else { TEXT_SOFT_L }
}
pub fn border() -> egui::Stroke {
    egui::Stroke::new(
        1.0,
        if is_dark() {
            BORDER_COLOR_D
        } else {
            BORDER_COLOR_L
        },
    )
}

// ---- 主题应用 ----
/// 应用全局主题，同时为深/亮主题分别注册 Style 与 Visuals。
pub fn apply_theme(ctx: &egui::Context, is_dark_theme: bool) {
    set_dark(is_dark_theme);
    let (dark_vis, light_vis) = (dark_visuals(), light_visuals());
    ctx.set_style_of(egui::Theme::Dark, base_style(dark_vis.clone()));
    ctx.set_style_of(egui::Theme::Light, base_style(light_vis.clone()));
    ctx.set_visuals_of(egui::Theme::Dark, dark_vis);
    ctx.set_visuals_of(egui::Theme::Light, light_vis);
    ctx.set_theme(if is_dark_theme {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
}

fn base_style(visuals: egui::Visuals) -> egui::Style {
    let mut style = egui::Style {
        visuals,
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 6.0);
    style.spacing.interact_size = egui::vec2(48.0, 30.0);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.icon_width = 20.0;
    style.spacing.icon_width_inner = 7.0;
    style.spacing.icon_spacing = 8.0;
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.floating_width = 4.0;
    style.spacing.scroll.content_margin = egui::Margin::same(4);

    // 全局字号：正文与按钮 15px、辅助 13px、标题 19px，列表/表格行 15px。
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(19.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.5));
    style
}

fn dark_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL_D;
    v.window_fill = BG_D;
    v.extreme_bg_color = BG_D;
    v.faint_bg_color = INPUT_BG_D;
    v.code_bg_color = INPUT_BG_D;
    v.override_text_color = Some(TEXT_D);
    v.hyperlink_color = BLUE;
    v.warn_fg_color = egui::Color32::from_rgb(0xE3, 0xA5, 0x3C);
    v.error_fg_color = ERROR_RED;
    v.selection.bg_fill = BLUE_CHECK.linear_multiply(0.40);
    v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: egui::Color32::from_black_alpha(120),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: egui::Color32::from_black_alpha(120),
    };
    widget_visuals(&mut v, false);
    v
}

fn light_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = PANEL_L;
    v.window_fill = BG_L;
    v.extreme_bg_color = BG_L;
    v.faint_bg_color = INPUT_BG_L;
    v.code_bg_color = INPUT_BG_L;
    v.override_text_color = Some(TEXT_L);
    v.hyperlink_color = BLUE;
    v.warn_fg_color = egui::Color32::from_rgb(0xB8, 0x84, 0x0E);
    v.error_fg_color = ERROR_RED;
    v.selection.bg_fill = BLUE_CHECK.linear_multiply(0.25);
    v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: egui::Color32::from_black_alpha(25),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: egui::Color32::from_black_alpha(25),
    };
    widget_visuals(&mut v, true);
    v
}

fn widget_visuals(v: &mut egui::Visuals, light: bool) {
    let (border_color, inactive_bg, hovered_bg, active_bg, open_bg, text_color, input_bg, panel_bg) =
        if light {
            (
                BORDER_COLOR_L,
                egui::Color32::from_rgb(0xF7, 0xF8, 0xFA),
                egui::Color32::from_rgb(0xEE, 0xF0, 0xF4),
                egui::Color32::from_rgb(0xDA, 0xE9, 0xFA),
                egui::Color32::from_rgb(0xF7, 0xF8, 0xFA),
                TEXT_L,
                INPUT_BG_L,
                PANEL_L,
            )
        } else {
            (
                BORDER_COLOR_D,
                egui::Color32::from_rgb(0x22, 0x28, 0x32),
                egui::Color32::from_rgb(0x2A, 0x31, 0x3D),
                egui::Color32::from_rgb(0x20, 0x3B, 0x5E),
                egui::Color32::from_rgb(0x22, 0x28, 0x32),
                TEXT_D,
                INPUT_BG_D,
                PANEL_D,
            )
        };
    let border = egui::Stroke::new(1.0, border_color);
    let border_hover = egui::Stroke::new(1.0, BLUE_CHECK);
    let text_stroke = egui::Stroke::new(1.0, text_color);
    let radius = egui::CornerRadius::same(CORNER);
    v.widgets.noninteractive.bg_fill = input_bg;
    v.widgets.noninteractive.weak_bg_fill = panel_bg;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(
        1.0,
        if light {
            egui::Color32::from_rgb(0xD0, 0xD5, 0xDD)
        } else {
            egui::Color32::from_white_alpha(10)
        },
    );
    v.widgets.noninteractive.fg_stroke = text_stroke;
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.inactive.bg_fill = inactive_bg;
    v.widgets.inactive.weak_bg_fill = inactive_bg;
    v.widgets.inactive.bg_stroke = border;
    v.widgets.inactive.fg_stroke = text_stroke;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.hovered.bg_fill = hovered_bg;
    v.widgets.hovered.weak_bg_fill = hovered_bg;
    v.widgets.hovered.bg_stroke = border_hover;
    v.widgets.hovered.fg_stroke = text_stroke;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.active.bg_fill = active_bg;
    v.widgets.active.weak_bg_fill = active_bg;
    v.widgets.active.bg_stroke = border_hover;
    v.widgets.active.fg_stroke = text_stroke;
    v.widgets.active.corner_radius = radius;
    v.widgets.open.bg_fill = open_bg;
    v.widgets.open.weak_bg_fill = open_bg;
    v.widgets.open.bg_stroke = border;
    v.widgets.open.fg_stroke = text_stroke;
    v.widgets.open.corner_radius = radius;
}

// ---- 主题感知控件 ----
pub fn primary_widget(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .color(egui::Color32::WHITE)
            .strong(),
    )
    .fill(BLUE)
    .stroke(egui::Stroke::new(0.0, BLUE))
    .corner_radius(egui::CornerRadius::same(CORNER))
}

pub fn secondary_widget(label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(text()))
        .fill(btn_gray())
        .stroke(border())
        .corner_radius(egui::CornerRadius::same(CORNER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_themes_have_readable_selection_text() {
        let ctx = egui::Context::default();
        apply_theme(&ctx, true);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let visuals = &ctx.style_of(theme).visuals;
            assert_ne!(
                visuals.selection.bg_fill, visuals.selection.stroke.color,
                "选中文字颜色不能与高亮底色相同（theme={theme:?}）"
            );
        }
    }

    #[test]
    fn theme_getters_return_valid_colors() {
        for dark in [true, false] {
            set_dark(dark);
            assert_ne!(bg(), egui::Color32::TRANSPARENT);
            assert_ne!(panel(), egui::Color32::TRANSPARENT);
            assert_ne!(text(), egui::Color32::TRANSPARENT);
            assert_ne!(text_soft(), egui::Color32::TRANSPARENT);
        }
        set_dark(true);
    }
}
