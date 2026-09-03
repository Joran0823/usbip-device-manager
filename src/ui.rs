// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Rendering for the egui application.

use crate::app::{Action, App, RowFlags, Tab, ToastKind};
use crate::config::UsbDevice;
use crate::lang;
use eframe::egui::{
    self, Align, Align2, Button, Checkbox, CollapsingHeader, Color32, ComboBox, Context, Frame,
    Label, Layout, Panel, RichText, Sense, Ui, UiBuilder, ViewportCommand, Window,
};
use std::time::Duration;

const ROW_H: f32 = 30.0;
const CELL_SPACING: f32 = 8.0;

impl App {
    pub fn is_exiting(&self) -> bool {
        self.exiting
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.process_messages(ctx);
        self.flush_usb_change(ctx);
        if self.is_exiting() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if self.need_refresh && !self.is_busy() {
            self.need_refresh = false;
            self.start_refresh();
        }
        if self.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.process_messages(&ctx);
        self.flush_usb_change(&ctx);

        if self.is_exiting() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested {
            if self.close_to_tray_requested() && !self.is_hidden() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.mark_hidden();
            } else {
                self.exiting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
        }

        if self.need_refresh {
            self.need_refresh = false;
            self.start_refresh();
        }

        let mut actions: Vec<Action> = Vec::new();
        self.ui_top_bar(ui, &mut actions);

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Devices => self.devices_page(ui, &mut actions),
            Tab::Persisted => self.persisted_page(ui, &mut actions),
            Tab::AutoAttach => self.auto_attach_page(ui, &mut actions),
        });

        self.run_actions(std::mem::take(&mut actions), &ctx);

        self.ui_settings(&ctx);
        self.ui_dialog(&ctx);
        self.ui_toasts(&ctx);

        if self.is_busy() || !self.toasts.is_empty() || self.dialog.is_some() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }
}

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

impl App {
    fn ui_top_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        Panel::top("nav").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let tabs = [
                    (Tab::Devices, lang::t("Device")),
                    (Tab::Persisted, lang::t("Persisted")),
                    (Tab::AutoAttach, lang::t("AutoAttach")),
                ];
                for (tab, label) in tabs {
                    if ui.selectable_label(self.tab == tab, label).clicked() {
                        self.tab = tab;
                        actions.push(Action::Refresh);
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(lang::t("Exit")).clicked() {
                        actions.push(Action::Quit);
                    }
                    if ui.button(lang::t("Settings")).clicked() {
                        actions.push(Action::OpenSettings);
                    }
                    ui.separator();

                    let zh = lang::is_zh();
                    ui.menu_button(lang::t("Language"), |ui| {
                        if ui.selectable_label(zh, "中文").clicked() && !zh {
                            actions.push(Action::SetLang(true));
                        }
                        if ui.selectable_label(!zh, "English").clicked() && zh {
                            actions.push(Action::SetLang(false));
                        }
                    });

                    let dark = self.cfg.app_config.dark_mode;
                    let label = if dark {
                        lang::t("Light")
                    } else {
                        lang::t("Dark")
                    };
                    if ui.button(label).clicked() {
                        actions.push(Action::ToggleDark);
                    }

                    if self.is_busy() {
                        ui.spinner();
                    }
                    if ui
                        .add_enabled(!self.is_busy(), Button::new(lang::t("Refresh")))
                        .clicked()
                    {
                        actions.push(Action::Refresh);
                    }
                });
            });
            ui.add_space(4.0);
        });
    }
}

// ---------------------------------------------------------------------------
// Shared page scaffolding
// ---------------------------------------------------------------------------

impl App {
    fn page_title_bar(
        &self,
        ui: &mut Ui,
        title: String,
        actions: &mut Vec<Action>,
        extra: impl FnOnce(&mut Ui, &mut Vec<Action>),
    ) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong(RichText::new(title).size(18.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                extra(ui, actions);
            });
        });
        ui.separator();
    }

    fn empty_hint(&self, ui: &mut Ui, text: String) {
        ui.add_space(16.0);
        ui.weak(text);
    }
}

// ---------------------------------------------------------------------------
// Devices page
// ---------------------------------------------------------------------------

impl App {
    fn devices_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let hidden = self.hidden_count();
        self.page_title_bar(ui, lang::t("WindowsUSBDevice"), actions, |_, _| {});

        if self.show_filtered() && hidden > 0 {
            ui.label(
                RichText::new(lang::t("ShowingHiddenHint"))
                    .small()
                    .italics()
                    .color(ui.visuals().warn_fg_color),
            );
            ui.add_space(2.0);
        }

        if !self.is_initialized() {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(lang::t("Loading"));
            });
            return;
        }
        if self.devices().is_empty() {
            self.empty_hint(ui, lang::t("NoConnectedDevice"));
            return;
        }

        let rows = self.devices().to_vec();
        let widths = table_widths(ui);
        let busy = self.is_busy();
        let show_filtered = self.show_filtered();
        let selected = self.selected_hwid(Tab::Devices).unwrap_or_default();

        header_row(
            ui,
            &[
                lang::t("AutoAttach"),
                lang::t("BusID"),
                lang::t("HardwareID"),
                lang::t("Bound"),
                lang::t("Attached"),
                lang::t("ForceBind"),
                lang::t("Description"),
            ],
            &widths,
        );
        ui.separator();

        // 设备列表：高度按内容自适应，最大不超过窗口可用高度的 2/3，
        // 超过上限才出现滚动条。
        let has_info = self.current_device().is_some();
        let avail_h = ui.available_height();
        let window_cap = ui.ctx().content_rect().height() * 2.0 / 3.0;
        let list_cap = (avail_h - if has_info { 100.0 } else { 0.0 })
            .min(window_cap)
            .max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("devices-list")
            .max_height(list_cap)
            .show(ui, |ui| {
                let mut idx = 0usize;
                for row in &rows {
                    if !row.is_visible(show_filtered) {
                        continue;
                    }
                    device_row(
                        ui,
                        idx,
                        row,
                        row.dev.hardware_id == selected,
                        &widths,
                        busy,
                        show_filtered,
                        actions,
                    );
                    idx += 1;
                }
            });

        // 设备信息：占据列表下方剩余的整块高度，滚动条随内容自动出现。
        if let Some(dev) = self.current_device().cloned() {
            ui.add_space(10.0);
            let info_h = ui.available_height().max(80.0);
            egui::ScrollArea::vertical()
                .id_salt("devices-info")
                .max_height(info_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    device_info_view(ui, &dev);
                });
        }
    }
}

fn table_widths(ui: &Ui) -> Vec<f32> {
    let avail = ui.available_width();
    let fixed = [58.0, 100.0, 196.0, 62.0, 66.0, 70.0];
    let fixed_sum: f32 = fixed.iter().sum();
    let spacing_sum = CELL_SPACING * (fixed.len() as f32);
    let desc = (avail - fixed_sum - spacing_sum - 24.0).max(150.0);
    fixed.iter().copied().chain(std::iter::once(desc)).collect()
}

fn header_row(ui: &mut Ui, headers: &[String], widths: &[f32]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = CELL_SPACING;
        for (i, h) in headers.iter().enumerate() {
            ui.add_sized(
                [widths.get(i).copied().unwrap_or(80.0), ROW_H],
                Label::new(RichText::new(h).strong()),
            );
        }
    });
}

fn text_cell(ui: &mut Ui, w: f32, text: &str, _selected: bool, dimmed: bool) -> egui::Response {
    let color = if dimmed {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().text_color()
    };
    ui.add_sized(
        [w, ROW_H],
        Label::new(RichText::new(text).color(color))
            .truncate()
            .sense(Sense::click()),
    )
}

fn cb_cell(ui: &mut Ui, w: f32, checked: bool, enabled: bool) -> (bool, bool) {
    let mut value = checked;
    let mut changed = false;
    ui.add_enabled_ui(enabled, |ui| {
        let r = ui.add_sized([w, ROW_H], Checkbox::new(&mut value, ""));
        changed = r.changed();
    });
    (changed, value)
}

fn device_row(
    ui: &mut Ui,
    idx: usize,
    row: &crate::app::DevRow,
    selected: bool,
    widths: &[f32],
    busy: bool,
    show_filtered: bool,
    actions: &mut Vec<Action>,
) {
    let dimmed = row.is_filtered;
    let auto = row.is_auto;
    let bound = row.dev.is_bound;
    let attached = row.dev.is_attached;
    let hwid = row.dev.hardware_id.clone();
    let flags = RowFlags {
        auto_enabled: !busy && !dimmed,
        bind_enabled: !busy && !dimmed && (!auto || !bound),
        attach_enabled: !busy && !dimmed && if auto { !attached } else { bound },
        force_enabled: !busy && !dimmed && !auto && !bound,
    };

    let total_w = widths.iter().sum::<f32>() + CELL_SPACING * (widths.len() as f32 - 1.0);
    let w = ui.available_width().min(total_w).max(240.0);
    let (rect, row_resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());

    let visuals = ui.visuals().clone();
    let bg = if selected {
        visuals.selection.bg_fill
    } else if idx % 2 == 1 {
        visuals.faint_bg_color
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);

    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CELL_SPACING;

            // Auto attach
            let (changed, value) = cb_cell(ui, widths[0], auto, flags.auto_enabled);
            if changed {
                if value {
                    actions.push(Action::AddAuto(hwid.clone()));
                } else {
                    actions.push(Action::RemoveAuto(hwid.clone()));
                }
            }

            // Bus ID
            let r = text_cell(ui, widths[1], &row.dev.bus_id, selected, dimmed);
            select_and_menu(ui, r, &hwid, row, &flags, selected, actions, Tab::Devices);

            // Hardware ID
            let r = text_cell(ui, widths[2], &row.dev.hardware_id, selected, dimmed);
            select_and_menu(ui, r, &hwid, row, &flags, selected, actions, Tab::Devices);

            // Bound
            let (changed, value) = cb_cell(ui, widths[3], bound, flags.bind_enabled);
            if changed {
                if value {
                    actions.push(Action::Bind(hwid.clone()));
                } else {
                    actions.push(Action::Unbind(hwid.clone()));
                }
            }

            // Attached
            let (changed, value) = cb_cell(ui, widths[4], attached, flags.attach_enabled);
            if changed {
                if value {
                    actions.push(Action::Attach(hwid.clone()));
                } else {
                    actions.push(Action::Detach(hwid.clone()));
                }
            }

            // Force (read-only display)
            let _ = cb_cell(ui, widths[5], row.dev.is_forced, false);

            // Description
            let r = text_cell(ui, widths[6], &row.dev.description, selected, dimmed);
            select_and_menu(ui, r, &hwid, row, &flags, selected, actions, Tab::Devices);
        });
    });

    if row_resp.clicked() || row_resp.secondary_clicked() {
        actions.push(Action::Select(Tab::Devices, hwid.clone()));
    }
    row_resp.context_menu(|ui| {
        device_menu(ui, actions, row, &flags, dimmed, auto, bound, attached);
    });
    let _ = show_filtered;
}

#[allow(clippy::too_many_arguments)]
fn select_and_menu(
    _ui: &mut Ui,
    resp: egui::Response,
    hwid: &str,
    row: &crate::app::DevRow,
    flags: &RowFlags,
    selected: bool,
    actions: &mut Vec<Action>,
    tab: Tab,
) {
    if !selected && (resp.clicked() || resp.secondary_clicked()) {
        actions.push(Action::Select(tab, hwid.to_owned()));
    }
    resp.context_menu(|ui| {
        device_menu(
            ui,
            actions,
            row,
            flags,
            row.is_filtered,
            row.is_auto,
            row.dev.is_bound,
            row.dev.is_attached,
        );
    });
}

fn device_menu(
    ui: &mut Ui,
    actions: &mut Vec<Action>,
    row: &crate::app::DevRow,
    flags: &RowFlags,
    dimmed: bool,
    auto: bool,
    bound: bool,
    attached: bool,
) {
    let hwid = &row.dev.hardware_id;
    if ui
        .add_enabled(flags.bind_enabled, Button::new(lang::t("Bind")))
        .clicked()
    {
        actions.push(Action::Bind(hwid.clone()));
    }
    if ui
        .add_enabled(bound && !auto, Button::new(lang::t("Unbind")))
        .clicked()
    {
        actions.push(Action::Unbind(hwid.clone()));
    }
    ui.separator();
    if ui
        .add_enabled(
            flags.attach_enabled && !auto,
            Button::new(lang::t("Attach")),
        )
        .clicked()
    {
        actions.push(Action::Attach(hwid.clone()));
    }
    if ui
        .add_enabled(attached && !auto && !dimmed, Button::new(lang::t("Detach")))
        .clicked()
    {
        actions.push(Action::Detach(hwid.clone()));
    }
    ui.separator();
    if ui
        .add_enabled(!dimmed && !auto, Button::new(lang::t("Hide")))
        .clicked()
    {
        actions.push(Action::Hide(hwid.clone()));
    }
    if ui
        .add_enabled(dimmed, Button::new(lang::t("Show")))
        .clicked()
    {
        actions.push(Action::Show(hwid.clone()));
    }
    if ui.button(lang::t("ShowHide")).clicked() {
        actions.push(Action::ToggleFilter);
    }
    ui.separator();
    if ui
        .add_enabled(!auto && !dimmed, Button::new(lang::t("AddToAuto")))
        .clicked()
    {
        actions.push(Action::AddAuto(hwid.clone()));
    }
    if ui
        .add_enabled(auto && !dimmed, Button::new(lang::t("RemoveFromAuto")))
        .clicked()
    {
        actions.push(Action::RemoveAuto(hwid.clone()));
    }
}

// ---------------------------------------------------------------------------
// Persisted devices page
// ---------------------------------------------------------------------------

impl App {
    fn persisted_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        self.page_title_bar(ui, lang::t("PersistedDevices"), actions, |ui, actions| {
            if ui
                .add_enabled(
                    !self.persisted().is_empty(),
                    Button::new(lang::t("DeleteAll")),
                )
                .clicked()
            {
                actions.push(Action::UnbindAllPersisted);
            }
        });

        if !self.is_initialized() {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(lang::t("Loading"));
            });
            return;
        }
        if self.persisted().is_empty() {
            self.empty_hint(ui, lang::t("NoPersistedDevice"));
            return;
        }

        let rows = self.persisted().to_vec();
        let busy = self.is_busy();
        let selected = self.selected_hwid(Tab::Persisted).unwrap_or_default();
        let widths = simple_widths(ui);

        header_row(
            ui,
            &[
                lang::t("HardwareID"),
                lang::t("PersistedGUID"),
                lang::t("Description"),
            ],
            &widths,
        );
        ui.separator();

        // 持久化设备列表：高度按内容自适应，最大不超过窗口可用高度的 2/3。
        let has_info = self.current_device().is_some();
        let avail_h = ui.available_height();
        let window_cap = ui.ctx().content_rect().height() * 2.0 / 3.0;
        let list_cap = (avail_h - if has_info { 100.0 } else { 0.0 })
            .min(window_cap)
            .max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("persisted-list")
            .max_height(list_cap)
            .show(ui, |ui| {
                for (i, dev) in rows.iter().enumerate() {
                    persisted_row(
                        ui,
                        i,
                        dev,
                        dev.hardware_id == selected,
                        &widths,
                        busy,
                        actions,
                    );
                }
            });

        // 设备信息：占据列表下方剩余的整块高度。
        if let Some(dev) = self.current_device().cloned() {
            ui.add_space(10.0);
            let info_h = ui.available_height().max(80.0);
            egui::ScrollArea::vertical()
                .id_salt("persisted-info")
                .max_height(info_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    device_info_view(ui, &dev);
                });
        }
    }
}

fn simple_widths(ui: &Ui) -> Vec<f32> {
    let avail = ui.available_width();
    let fixed = [210.0, 250.0];
    let desc = (avail - fixed.iter().sum::<f32>() - CELL_SPACING * 2.0 - 24.0).max(150.0);
    vec![fixed[0], fixed[1], desc]
}

fn persisted_row(
    ui: &mut Ui,
    idx: usize,
    dev: &UsbDevice,
    selected: bool,
    widths: &[f32],
    busy: bool,
    actions: &mut Vec<Action>,
) {
    let hwid = dev.hardware_id.clone();
    let total_w = widths.iter().sum::<f32>() + CELL_SPACING * (widths.len() as f32 - 1.0);
    let w = ui.available_width().min(total_w).max(240.0);
    let (rect, row_resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
    let visuals = ui.visuals().clone();
    let bg = if selected {
        visuals.selection.bg_fill
    } else if idx % 2 == 1 {
        visuals.faint_bg_color
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);

    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CELL_SPACING;
            let r1 = text_cell(ui, widths[0], &dev.hardware_id, selected, false);
            let r2 = text_cell(ui, widths[1], &dev.persisted_guid, selected, false);
            let r3 = text_cell(ui, widths[2], &dev.description, selected, false);
            for r in [r1, r2, r3] {
                if !selected && (r.clicked() || r.secondary_clicked()) {
                    actions.push(Action::Select(Tab::Persisted, hwid.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button(lang::t("DeleteSelected")).clicked() {
                        actions.push(Action::UnbindPersisted(hwid.clone()));
                    }
                });
            }
        });
    });
    if row_resp.clicked() || row_resp.secondary_clicked() {
        actions.push(Action::Select(Tab::Persisted, hwid.clone()));
    }
    row_resp.context_menu(|ui| {
        if ui
            .add_enabled(!busy, Button::new(lang::t("DeleteSelected")))
            .clicked()
        {
            actions.push(Action::UnbindPersisted(hwid.clone()));
        }
        if ui
            .add_enabled(!busy, Button::new(lang::t("DeleteAll")))
            .clicked()
        {
            actions.push(Action::UnbindAllPersisted);
        }
    });
}

// ---------------------------------------------------------------------------
// Auto attach page
// ---------------------------------------------------------------------------

impl App {
    fn auto_attach_page(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        self.page_title_bar(
            ui,
            lang::t("AutoAttachHeaderTitle"),
            actions,
            |ui, actions| {
                if ui
                    .add_enabled(
                        !self.auto_devices().is_empty(),
                        Button::new(lang::t("DeleteAll")),
                    )
                    .clicked()
                {
                    actions.push(Action::RemoveAllAuto);
                }
            },
        );

        if !self.is_initialized() {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(lang::t("Loading"));
            });
            return;
        }
        if self.auto_devices().is_empty() {
            self.empty_hint(ui, lang::t("NoAutoDevice"));
            return;
        }

        let rows = self.auto_devices().to_vec();
        let selected = self.selected_hwid(Tab::AutoAttach).unwrap_or_default();
        let widths = simple_widths(ui);

        header_row(
            ui,
            &[
                lang::t("HardwareID"),
                lang::t("PersistedGUID"),
                lang::t("Description"),
            ],
            &widths,
        );
        ui.separator();

        // 自动附加列表：占满剩余高度，滚动条随内容自动出现。
        egui::ScrollArea::vertical()
            .id_salt("auto-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (i, dev) in rows.iter().enumerate() {
                    auto_row(ui, i, dev, dev.hardware_id == selected, &widths, actions);
                }
            });
    }
}

fn auto_row(
    ui: &mut Ui,
    idx: usize,
    dev: &UsbDevice,
    selected: bool,
    widths: &[f32],
    actions: &mut Vec<Action>,
) {
    let hwid = dev.hardware_id.clone();
    let total_w = widths.iter().sum::<f32>() + CELL_SPACING * (widths.len() as f32 - 1.0);
    let w = ui.available_width().min(total_w).max(240.0);
    let (rect, row_resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
    let visuals = ui.visuals().clone();
    let bg = if selected {
        visuals.selection.bg_fill
    } else if idx % 2 == 1 {
        visuals.faint_bg_color
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);

    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CELL_SPACING;
            let r1 = text_cell(ui, widths[0], &dev.hardware_id, selected, false);
            let r2 = text_cell(ui, widths[1], &dev.persisted_guid, selected, false);
            let r3 = text_cell(ui, widths[2], &dev.description, selected, false);
            for r in [r1, r2, r3] {
                if !selected && (r.clicked() || r.secondary_clicked()) {
                    actions.push(Action::Select(Tab::AutoAttach, hwid.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button(lang::t("DeleteSelected")).clicked() {
                        actions.push(Action::RemoveAutoDevice(hwid.clone()));
                    }
                });
            }
        });
    });
    if row_resp.clicked() || row_resp.secondary_clicked() {
        actions.push(Action::Select(Tab::AutoAttach, hwid.clone()));
    }
    row_resp.context_menu(|ui| {
        if ui.button(lang::t("DeleteSelected")).clicked() {
            actions.push(Action::RemoveAutoDevice(hwid.clone()));
        }
        if ui.button(lang::t("DeleteAll")).clicked() {
            actions.push(Action::RemoveAllAuto);
        }
    });
}

// ---------------------------------------------------------------------------
// Device details
// ---------------------------------------------------------------------------

fn device_info_view(ui: &mut Ui, dev: &UsbDevice) {
    CollapsingHeader::new(RichText::new(lang::t("DeviceInfo")).strong())
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new("device-info-grid")
                .num_columns(2)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    info_row(ui, lang::t("InstanceID"), &dev.instance_id);
                    info_row(ui, lang::t("HardwareID"), &dev.hardware_id);
                    info_row(ui, lang::t("BusID"), &dev.bus_id);
                    info_row(ui, lang::t("Description"), &dev.description);
                    info_row(
                        ui,
                        lang::t("Connected"),
                        bool_text(dev.is_connected).as_str(),
                    );
                    info_row(ui, lang::t("ForceBind"), bool_text(dev.is_forced).as_str());
                    info_row(ui, lang::t("Bound"), bool_text(dev.is_bound).as_str());
                    info_row(ui, lang::t("Attached"), bool_text(dev.is_attached).as_str());
                    info_row(ui, lang::t("ClientIP"), &dev.client_ip_address);
                    info_row(ui, lang::t("PersistedGUID"), &dev.persisted_guid);
                    info_row(ui, lang::t("StubInstanceID"), &dev.stub_instance_id);
                });
        });
}

fn bool_text(b: bool) -> String {
    if b {
        "True".to_owned()
    } else {
        "False".to_owned()
    }
}

fn info_row(ui: &mut Ui, label: String, value: &str) {
    ui.label(RichText::new(format!("{label}:")).weak());
    ui.add(Label::new(value).wrap().sense(Sense::click()));
    ui.end_row();
}

// ---------------------------------------------------------------------------
// Settings dialog
// ---------------------------------------------------------------------------

impl App {
    fn ui_settings(&mut self, ctx: &Context) {
        if !self.settings_visible() {
            return;
        }
        let mut close = false;
        let mut ok = false;
        let mut reset = false;
        let mut clear_logs = false;
        let mut open_dir = false;

        let draft = self.settings_draft().clone();
        let netcard_names: Vec<String> = self
            .netcards()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let (mut specify, mut tray, mut use_bus, mut card) = (
            draft.specify_net_card,
            draft.close_to_tray,
            draft.use_bus_id,
            draft.forward_net_card.clone(),
        );
        let mut specify_changed = false;
        let mut tray_changed = false;
        let mut bus_changed = false;
        let mut card_changed = false;

        Window::new(lang::t("Settings"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.add_space(4.0);

                let resp = ui
                    .checkbox(&mut specify, lang::t("SpecifyNetworkCard"))
                    .on_hover_text(lang::t("ToolTipSpecifyNetworkCard"));
                if resp.changed() {
                    specify_changed = true;
                    if !specify {
                        card.clear();
                        card_changed = true;
                    }
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.add_enabled_ui(specify, |ui| {
                        ComboBox::from_id_salt("netcard")
                            .selected_text(if card.is_empty() {
                                lang::t("SelectNetCart")
                            } else {
                                card.clone()
                            })
                            .width(340.0)
                            .show_ui(ui, |ui| {
                                for name in &netcard_names {
                                    if ui.selectable_label(card == *name, name).clicked() {
                                        card = name.clone();
                                        card_changed = true;
                                    }
                                }
                            });
                    });
                });
                ui.add_space(8.0);

                let r1 = ui.checkbox(&mut tray, lang::t("ClosToTray")).changed();
                if r1 {
                    tray_changed = true;
                }
                let r2 = ui
                    .checkbox(&mut use_bus, lang::t("UseBusID"))
                    .on_hover_text(lang::t("ToolTipUseBusID"))
                    .changed();
                if r2 {
                    bus_changed = true;
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    if ui.button(lang::t("ResetConfig")).clicked() {
                        reset = true;
                    }
                    if ui.button(lang::t("ClearLogFiles")).clicked() {
                        clear_logs = true;
                    }
                    if ui.button(lang::t("OpenCinfigPath")).clicked() {
                        open_dir = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(lang::t("OK")).clicked() {
                            ok = true;
                        }
                        if ui.button(lang::t("Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            });

        if specify_changed || tray_changed || bus_changed || card_changed {
            self.update_settings_draft(move |d| {
                d.specify_net_card = specify;
                d.close_to_tray = tray;
                d.use_bus_id = use_bus;
                d.forward_net_card = card;
            });
        }
        if reset {
            self.reset_config();
        }
        if clear_logs {
            self.clear_logs();
        }
        if open_dir {
            self.open_config_dir();
        }
        if ok {
            self.settings_ok();
        }
        if close {
            self.close_settings();
        }
    }
}

// ---------------------------------------------------------------------------
// Error dialog and toasts
// ---------------------------------------------------------------------------

impl App {
    fn ui_dialog(&mut self, ctx: &Context) {
        let Some(dlg) = self.dialog.clone() else {
            return;
        };
        Window::new(dlg.title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.add_space(4.0);
                ui.add(Label::new(&dlg.message).wrap().sense(Sense::hover()));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(lang::t("OK")).clicked() {
                            self.dialog_mut().take();
                        }
                    });
                });
            });
    }

    fn ui_toasts(&mut self, ctx: &Context) {
        let toasts: Vec<(usize, crate::app::ToastKind, String)> = self
            .toasts
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.kind, t.text.clone()))
            .collect();
        if toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toast-area"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -14.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    for (i, kind, text) in toasts {
                        let color = match kind {
                            ToastKind::Info => ui.visuals().text_color(),
                            ToastKind::Success => crate::theme::OK_GREEN,
                            ToastKind::Error => crate::theme::ERROR_RED,
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.colored_label(color, "●");
                            ui.label(text);
                            ui.add_space(6.0);
                            if ui.small_button("x").clicked() {
                                self.toasts.remove(i);
                            }
                        });
                    }
                });
            });
    }
}
