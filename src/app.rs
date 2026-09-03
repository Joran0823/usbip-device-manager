// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

#![allow(dead_code)]

//! Application state, background workers and action dispatch.

use crate::config::{self, AppConfig, SystemConfig, UsbDevice};
use crate::lang;
use crate::log;
use crate::monitor::{UsbChange, UsbMonitor};
use crate::usbipd::{self, DaemonManager, Usbipd};
use eframe::egui;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Devices,
    Persisted,
    AutoAttach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub expires: Instant,
}

#[derive(Debug, Clone)]
pub enum UiMsg {
    Error(String),
    Lists(Vec<UsbDevice>),
    OpDone { ok: bool, message: String },
    UsbNotify(Option<String>),
    UsbChanged(UsbChange),
    UsbListFinished,
    ShowWindow,
    ExitApp,
    UsbipdReady(Result<(Usbipd, Option<String>), String>),
}

#[derive(Debug, Clone)]
pub struct DevRow {
    pub dev: UsbDevice,
    pub is_auto: bool,
    pub is_filtered: bool,
}

impl DevRow {
    pub fn is_visible(&self, show_filtered: bool) -> bool {
        show_filtered || !self.is_filtered
    }
}

#[derive(Debug, Clone, Default)]
pub struct Dialog {
    pub title: String,
    pub message: String,
}

/// Actions queued by widgets and executed after the current UI pass.
#[derive(Debug, Clone)]
pub enum Action {
    Select(Tab, String),
    ToggleFilter,
    ToggleDark,
    SetLang(bool),
    OpenSettings,
    Quit,
    Bind(String),
    Unbind(String),
    Attach(String),
    Detach(String),
    AddAuto(String),
    RemoveAuto(String),
    Hide(String),
    Show(String),
    UnbindPersisted(String),
    UnbindAllPersisted,
    RemoveAutoDevice(String),
    RemoveAllAuto,
    Refresh,
}

pub struct App {
    pub(crate) cfg: SystemConfig,
    usbipd: Option<Usbipd>,
    netcards: Vec<(String, String)>,

    pub tab: Tab,
    device_rows: Vec<DevRow>,
    persisted_rows: Vec<UsbDevice>,
    auto_rows: Vec<UsbDevice>,
    selected_hwid: HashMap<Tab, String>,
    show_filtered: bool,

    busy: bool,
    initialized: bool,
    pub(crate) need_refresh: bool,
    pending_attach: HashMap<String, Instant>,
    pending_usb: Option<UsbChange>,
    last_usb_refresh: Option<Instant>,
    usb_listing: bool,

    show_settings: bool,
    settings_draft: AppConfig,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) dialog: Option<Dialog>,

    rx: Receiver<UiMsg>,
    tx: Sender<UiMsg>,
    daemons: Arc<Mutex<DaemonManager>>,
    monitor: Option<UsbMonitor>,
    pub(crate) exiting: bool,
    hidden: bool,
    last_title: Option<String>,
    tray: Option<tray_icon::TrayIcon>,
    tray_show: Option<tray_icon::menu::MenuItem>,
    tray_exit: Option<tray_icon::menu::MenuItem>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cfg = config::load();
        let zh = if cfg.app_config.lang.is_empty() {
            crate::sys::system_locale_is_chinese()
        } else {
            cfg.app_config.lang.eq_ignore_ascii_case("zh")
        };
        lang::set_zh(zh);

        let (tx, rx) = std::sync::mpsc::channel();

        let mut app = Self {
            cfg,
            usbipd: None,
            netcards: Vec::new(),
            tab: Tab::Devices,
            device_rows: Vec::new(),
            persisted_rows: Vec::new(),
            auto_rows: Vec::new(),
            selected_hwid: HashMap::new(),
            show_filtered: false,
            busy: true,
            initialized: false,
            need_refresh: false,
            pending_attach: HashMap::new(),
            pending_usb: None,
            last_usb_refresh: None,
            usb_listing: false,
            show_settings: false,
            settings_draft: AppConfig::default(),
            toasts: Vec::new(),
            dialog: None,
            rx,
            tx,
            daemons: Arc::new(Mutex::new(DaemonManager::new())),
            monitor: None,
            exiting: false,
            hidden: false,
            last_title: None,
            tray: None,
            tray_show: None,
            tray_exit: None,
        };

        app.apply_theme(&cc.egui_ctx);
        if let Some(icon) = load_window_icon() {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(icon))));
        }

        let tx2 = app.tx.clone();
        std::thread::Builder::new()
            .name("usbipd-check".to_owned())
            .spawn(move || {
                let _ = tx2.send(UiMsg::UsbipdReady(Usbipd::check()));
            })?;

        let tx4 = app.tx.clone();
        let egui_ctx = cc.egui_ctx.clone();
        app.monitor = Some(UsbMonitor::start(move |change| {
            let _ = tx4.send(UiMsg::UsbChanged(change));
            egui_ctx.request_repaint();
        }));
        match build_tray(app.tx.clone(), cc.egui_ctx.clone()) {
            Ok(Some((tray, show, exit))) => {
                app.tray = Some(tray);
                app.tray_show = Some(show);
                app.tray_exit = Some(exit);
            }
            Ok(None) => {}
            Err(e) => {
                log::warn(&format!(
                    "Failed to create tray icon (continuing without it): {e}"
                ));
            }
        }

        Ok(app)
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn apply_theme(&mut self, ctx: &egui::Context) {
        crate::theme::apply_theme(ctx, self.cfg.app_config.dark_mode);
        let zh = if self.cfg.app_config.lang.is_empty() {
            lang::is_zh()
        } else {
            self.cfg.app_config.lang.eq_ignore_ascii_case("zh")
        };
        lang::set_zh(zh);
    }

    pub fn save_cfg(&mut self) {
        if self.cfg.app_config.lang.is_empty() {
            self.cfg.app_config.lang = if lang::is_zh() { "zh" } else { "en" }.to_owned();
        }
        config::save(&self.cfg);
    }

    pub fn window_title(&self) -> String {
        format!("{} {}", lang::t("WindowTitle"), crate::APP_VERSION)
    }

    // ------------------------------------------------------------------
    // Message processing
    // ------------------------------------------------------------------

    pub(crate) fn process_messages(&mut self, ctx: &egui::Context) {
        let mut messages: Vec<UiMsg> = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            messages.push(msg);
        }
        for msg in messages {
            match msg {
                UiMsg::Error(text) => {
                    log::error(&text);
                    self.dialog = Some(Dialog {
                        title: lang::t("Error"),
                        message: text,
                    });
                }
                UiMsg::Lists(devices) => {
                    self.busy = false;
                    self.apply_lists(devices);
                    ctx.request_repaint();
                }
                UiMsg::UsbNotify(text) => {
                    if let Some(text) = text {
                        self.toasts.push(Toast {
                            kind: ToastKind::Info,
                            text,
                            expires: Instant::now() + Duration::from_secs(5),
                        });
                    }
                }
                UiMsg::UsbChanged(change) => {
                    self.queue_usb_change(change, ctx);
                }
                UiMsg::UsbListFinished => {
                    self.usb_listing = false;
                    ctx.request_repaint();
                }
                UiMsg::OpDone { ok, message } => {
                    self.busy = false;
                    if ok {
                        self.toasts.push(Toast {
                            kind: ToastKind::Success,
                            text: message,
                            expires: Instant::now() + Duration::from_secs(4),
                        });
                    } else if !message.is_empty() {
                        self.dialog = Some(Dialog {
                            title: lang::t("Error"),
                            message,
                        });
                    }
                    self.need_refresh = true;
                    ctx.request_repaint();
                }
                UiMsg::ShowWindow => {
                    self.hidden = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.request_repaint();
                }
                UiMsg::ExitApp => {
                    self.exiting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                UiMsg::UsbipdReady(Ok((client, warning))) => {
                    log::info(&format!("usbipd-win {} found.", client.version));
                    self.usbipd = Some(client);
                    self.busy = false;
                    if let Some(w) = warning {
                        self.toasts.push(Toast {
                            kind: ToastKind::Info,
                            text: w,
                            expires: Instant::now() + Duration::from_secs(8),
                        });
                    }
                    self.start_refresh();
                }
                UiMsg::UsbipdReady(Err(e)) => {
                    self.busy = false;
                    self.dialog = Some(Dialog {
                        title: lang::t("UsbipdCheckFailed"),
                        message: e,
                    });
                }
            }
        }
        self.toasts.retain(|t| t.expires > Instant::now());
    }

    fn apply_lists(&mut self, all: Vec<UsbDevice>) {
        let connected: Vec<UsbDevice> = all.iter().filter(|d| d.is_connected).cloned().collect();
        let persisted: Vec<UsbDevice> = all
            .iter()
            .filter(|d| !d.is_connected && d.is_bound)
            .cloned()
            .collect();

        self.device_rows = connected
            .into_iter()
            .map(|dev| DevRow {
                is_auto: self.cfg.is_in_auto_list(&dev.hardware_id),
                is_filtered: self.cfg.is_in_filter_list(&dev.hardware_id),
                dev,
            })
            .collect();
        self.persisted_rows = persisted;

        let mut auto = Vec::with_capacity(self.cfg.auto_attach_device_list.len());
        for cfg_dev in &self.cfg.auto_attach_device_list {
            let live = all
                .iter()
                .find(|d| d.hardware_id.eq_ignore_ascii_case(&cfg_dev.hardware_id));
            auto.push(match live {
                Some(live) => {
                    let mut m = live.clone();
                    m.persisted_guid = cfg_dev.persisted_guid.clone();
                    m.description = cfg_dev.description.clone();
                    m
                }
                None => cfg_dev.clone(),
            });
        }
        self.auto_rows = auto;
        self.restore_selections();
        self.initialized = true;
        self.maybe_auto_attach();
    }

    fn restore_selections(&mut self) {
        let hwid = |rows: &[&UsbDevice]| {
            rows.first()
                .map(|d| d.hardware_id.clone())
                .unwrap_or_default()
        };
        for tab in [Tab::Devices, Tab::Persisted, Tab::AutoAttach] {
            let rows: Vec<&UsbDevice> = match tab {
                Tab::Devices => self
                    .device_rows
                    .iter()
                    .filter(|r| r.is_visible(self.show_filtered))
                    .map(|r| &r.dev)
                    .collect(),
                Tab::Persisted => self.persisted_rows.iter().collect(),
                Tab::AutoAttach => self.auto_rows.iter().collect(),
            };
            let current = self.selected_hwid.get(&tab).cloned().unwrap_or_default();
            if !current.is_empty() && rows.iter().any(|d| d.hardware_id == current) {
                continue;
            }
            self.selected_hwid.insert(tab, hwid(&rows));
        }
    }

    /// Auto attach connected devices listed in the auto-attach profile
    /// (mirrors `USBDevicesViewModel.UpdateDevices`), throttled.
    fn maybe_auto_attach(&mut self) {
        if !self.initialized || self.busy {
            return;
        }
        let now = Instant::now();
        let mut candidate: Option<UsbDevice> = None;
        for row in &self.device_rows {
            if !row.is_auto || !row.dev.is_connected || row.dev.is_attached {
                continue;
            }
            let id = if self.cfg.app_config.use_bus_id {
                row.dev.bus_id.clone()
            } else {
                row.dev.hardware_id.clone()
            };
            let attaching = self
                .daemons
                .lock()
                .map(|mut dm| dm.attaching(&id))
                .unwrap_or(false);
            let recent = self
                .pending_attach
                .get(&row.dev.hardware_id)
                .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(8));
            if attaching || recent {
                continue;
            }
            candidate = Some(row.dev.clone());
            break;
        }
        if let Some(dev) = candidate {
            self.pending_attach.insert(dev.hardware_id.clone(), now);
            self.attach(dev, true);
        }
    }

    // ------------------------------------------------------------------
    // Workers
    // ------------------------------------------------------------------

    pub fn start_refresh(&mut self) {
        if self.busy {
            self.need_refresh = true;
            return;
        }
        let Some(client) = self.usbipd.clone() else {
            self.busy = false;
            return;
        };
        self.busy = true;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("refresh".to_owned())
            .spawn(move || match client.list_devices() {
                Ok(list) => {
                    let _ = tx.send(UiMsg::Lists(list));
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::Error(e));
                    let _ = tx.send(UiMsg::Lists(Vec::new()));
                }
            })
            .expect("spawn refresh thread");
    }

    fn run_op(
        &mut self,
        f: impl FnOnce(&mut dyn FnMut(Result<String, String>)) + Send + 'static,
        success_msg: String,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("usbipd-op".to_owned())
            .spawn(move || {
                let mut sink = |r: Result<String, String>| {
                    let (ok, message) = match r {
                        Ok(m) => (true, if m.is_empty() { success_msg.clone() } else { m }),
                        Err(e) => (false, e),
                    };
                    let _ = tx.send(UiMsg::OpDone { ok, message });
                };
                f(&mut sink);
            })
            .expect("spawn op thread");
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    pub fn run_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::Select(tab, hwid) => {
                    self.selected_hwid.insert(tab, hwid);
                }
                Action::ToggleFilter => {
                    self.show_filtered = !self.show_filtered;
                    self.restore_selections();
                }
                Action::ToggleDark => {
                    self.cfg.app_config.dark_mode = !self.cfg.app_config.dark_mode;
                    self.save_cfg();
                    self.apply_theme(ctx);
                }
                Action::SetLang(zh) => {
                    self.cfg.app_config.lang = if zh { "zh" } else { "en" }.to_owned();
                    self.save_cfg();
                    self.apply_theme(ctx);
                    self.update_tray_language();
                }
                Action::OpenSettings => self.open_settings(),
                Action::Quit => {
                    self.exiting = true;
                }
                Action::Refresh => {
                    // 刷新后隐藏设备保持隐藏（回到默认的“仅显示可见设备”状态）。
                    self.show_filtered = false;
                    self.start_refresh();
                }
                Action::Bind(hwid) => self.bind_by_hwid(&hwid),
                Action::Unbind(hwid) => self.unbind_by_hwid(&hwid),
                Action::Attach(hwid) => {
                    if let Some(dev) = self.find_dev(&hwid).cloned() {
                        self.attach(dev, false);
                    }
                }
                Action::Detach(hwid) => {
                    if let Some(dev) = self.find_dev(&hwid).cloned() {
                        self.detach(dev);
                    }
                }
                Action::AddAuto(hwid) => self.add_auto_by_hwid(&hwid),
                Action::RemoveAuto(hwid) => self.remove_auto_by_hwid(&hwid),
                Action::Hide(hwid) => {
                    if let Some(dev) = self.find_dev(&hwid).cloned() {
                        self.cfg.add_to_filter_list(&dev);
                        self.save_cfg();
                        if let Some(row) = self
                            .device_rows
                            .iter_mut()
                            .find(|r| r.dev.hardware_id == hwid)
                        {
                            row.is_filtered = true;
                        }
                        self.toast_info(&format!("{} ({})", dev.short_name(), dev.hardware_id));
                    }
                }
                Action::Show(hwid) => {
                    if self.cfg.is_in_filter_list(&hwid) {
                        self.cfg.remove_from_filter_list(&hwid);
                        self.save_cfg();
                    }
                    if let Some(row) = self
                        .device_rows
                        .iter_mut()
                        .find(|r| r.dev.hardware_id == hwid)
                    {
                        row.is_filtered = false;
                    }
                }
                Action::UnbindPersisted(hwid) => {
                    if let Some(dev) = self.persisted_rows.iter().find(|d| d.hardware_id == hwid) {
                        self.unbind(dev.clone());
                    }
                }
                Action::UnbindAllPersisted => {
                    self.unbind_all_persisted();
                }
                Action::RemoveAutoDevice(hwid) => {
                    self.cfg.remove_from_auto_list(&hwid);
                    self.save_cfg();
                    self.auto_rows.retain(|d| d.hardware_id != hwid);
                    self.restore_selections();
                }
                Action::RemoveAllAuto => {
                    self.cfg.auto_attach_device_list.clear();
                    self.save_cfg();
                    self.auto_rows.clear();
                    self.restore_selections();
                }
            }
        }
        let title = self.window_title();
        if self.last_title.as_deref() != Some(title.as_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = Some(title);
        }
    }

    fn bind_by_hwid(&mut self, hwid: &str) {
        let Some(dev) = self.find_dev(hwid) else {
            return;
        };
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        let use_bus_id = self.cfg.app_config.use_bus_id;
        let dev2 = dev.clone();
        self.run_op(
            move |done| {
                let r = usbipd::bind_device(&client, &dev2, use_bus_id, dev2.is_forced)
                    .map(|_| String::new());
                done(r.map_err(|e| format!("{}: {e}", lang::t("ErrMsgBindFail"))));
            },
            lang::t("BindSuccess"),
        );
    }

    fn unbind_by_hwid(&mut self, hwid: &str) {
        let Some(dev) = self.find_dev(hwid) else {
            return;
        };
        let dev2 = dev.clone();
        self.unbind(dev2);
    }

    fn unbind(&mut self, dev: UsbDevice) {
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        let use_bus_id = self.cfg.app_config.use_bus_id;
        let daemons = self.daemons.clone();
        self.run_op(
            move |done| {
                let r = usbipd::unbind_device(&client, &dev, use_bus_id, &daemons)
                    .map(|_| String::new());
                done(r.map_err(|e| format!("{}: {e}", lang::t("ErrMsgUnbindFail"))));
            },
            lang::t("UnbindSuccess"),
        );
    }

    fn unbind_all_persisted(&mut self) {
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        let devices = self.persisted_rows.clone();
        if devices.is_empty() {
            return;
        }
        let use_bus_id = self.cfg.app_config.use_bus_id;
        let daemons = self.daemons.clone();
        self.run_op(
            move |done| {
                let mut last_err: Option<String> = None;
                for dev in &devices {
                    if let Err(e) = usbipd::unbind_device(&client, dev, use_bus_id, &daemons) {
                        last_err = Some(e);
                    }
                }
                match last_err {
                    Some(e) => done(Err(format!("{}: {e}", lang::t("ErrMsgUnbindFail")))),
                    None => done(Ok(String::new())),
                }
            },
            lang::t("UnbindSuccess"),
        );
    }

    pub fn attach(&mut self, dev: UsbDevice, auto: bool) {
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        let cfg_snapshot = self.cfg.app_config.clone();
        let daemons = self.daemons.clone();
        self.run_op(
            move |done| {
                let host_ip = if cfg_snapshot.specify_net_card {
                    let cards = crate::sys::network_cards();
                    match cards
                        .iter()
                        .find(|(name, _)| *name == cfg_snapshot.forward_net_card)
                    {
                        Some((_, ip)) => Some(ip.clone()),
                        None => {
                            done(Err(format!(
                                "{}: {} ({})",
                                lang::t("ErrMsgAttachFail"),
                                lang::t("ErrNoNetCard"),
                                cfg_snapshot.forward_net_card
                            )));
                            return;
                        }
                    }
                } else {
                    None
                };
                let r = usbipd::attach_device(
                    &client,
                    &dev,
                    cfg_snapshot.use_bus_id,
                    auto,
                    host_ip.as_deref(),
                    &daemons,
                );
                done(r.map_err(|e| format!("{}: {e}", lang::t("ErrMsgAttachFail"))));
            },
            lang::t("AttachSuccess"),
        );
    }

    fn detach(&mut self, dev: UsbDevice) {
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        let use_bus_id = self.cfg.app_config.use_bus_id;
        let daemons = self.daemons.clone();
        self.run_op(
            move |done| {
                let r = usbipd::detach_device(&client, &dev, use_bus_id, &daemons)
                    .map(|_| String::new());
                done(r.map_err(|e| format!("{}: {e}", lang::t("ErrMsgDetachFail"))));
            },
            lang::t("DetachSuccess"),
        );
    }

    fn add_auto_by_hwid(&mut self, hwid: &str) {
        let Some(dev) = self.find_dev(hwid).cloned() else {
            return;
        };
        if self.cfg.is_in_auto_list(&dev.hardware_id) {
            return;
        }
        self.cfg.add_to_auto_list(&dev);
        self.save_cfg();
        if let Some(row) = self
            .device_rows
            .iter_mut()
            .find(|r| r.dev.hardware_id == hwid)
        {
            row.is_auto = true;
        }
        self.toast_info(&format!("'{}' added.", dev.short_name()));
        // 与 C# 版 AddToAutoAttach 一致：加入列表后立即进入附加流程。
        // 未绑定时由 attach_device 先执行 usbipd bind，再执行 attach。
        if dev.is_connected {
            self.attach(dev, true);
        }
    }

    fn remove_auto_by_hwid(&mut self, hwid: &str) {
        if !self.cfg.is_in_auto_list(hwid) {
            return;
        }
        let name = self
            .find_dev(hwid)
            .map(|d| d.short_name())
            .unwrap_or_else(|| hwid.to_owned());
        self.cfg.remove_from_auto_list(hwid);
        self.save_cfg();
        if let Some(row) = self
            .device_rows
            .iter_mut()
            .find(|r| r.dev.hardware_id == hwid)
        {
            row.is_auto = false;
        }
        self.auto_rows.retain(|d| d.hardware_id != hwid);
        self.toast_info(&format!("'{}' removed.", name));
    }

    pub fn find_dev(&self, hwid: &str) -> Option<&UsbDevice> {
        if self.tab == Tab::Devices {
            self.device_rows
                .iter()
                .find(|r| r.dev.hardware_id == hwid)
                .map(|r| &r.dev)
        } else {
            None
        }
    }

    fn toast_info(&mut self, text: &str) {
        self.toasts.push(Toast {
            kind: ToastKind::Info,
            text: text.to_owned(),
            expires: Instant::now() + Duration::from_secs(3),
        });
    }

    // ------------------------------------------------------------------
    // USB event handling (called from UI pass)
    // ------------------------------------------------------------------

    /// 接收 USB 插拔事件：短时间内的事件先合并，冷却结束后只刷新一次，
    /// 避免复合设备枚举时连续多次调用 usbipd state。
    pub fn queue_usb_change(&mut self, change: UsbChange, ctx: &egui::Context) {
        self.pending_usb = Some(change);
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    /// 每帧尝试处理合并后的 USB 事件（在 logic 与 ui 中调用）。
    pub fn flush_usb_change(&mut self, ctx: &egui::Context) {
        if self.busy || self.usb_listing {
            return;
        }
        let Some(change) = self.pending_usb.take() else {
            return;
        };
        // 若上一次刷新在 900ms 内，说明系统仍在枚举设备，继续合并等待。
        if self
            .last_usb_refresh
            .is_some_and(|t| t.elapsed() < Duration::from_millis(900))
        {
            self.pending_usb = Some(change);
            ctx.request_repaint_after(Duration::from_millis(150));
            return;
        }
        self.last_usb_refresh = Some(Instant::now());
        self.usb_listing = true;
        self.start_usb_list(change, ctx);
    }

    fn start_usb_list(&mut self, change: UsbChange, ctx: &egui::Context) {
        let Some(client) = self.usbipd.clone() else {
            self.usb_listing = false;
            return;
        };
        let tx = self.tx.clone();
        let egui_ctx = ctx.clone();
        let started = Instant::now();
        std::thread::Builder::new()
            .name("usb-event".to_owned())
            .spawn(move || match client.list_devices() {
                Ok(list) => {
                    log::info(&format!(
                        "USB change {} -> refreshed {} device(s) in {} ms",
                        change.hardware_id,
                        list.len(),
                        started.elapsed().as_millis()
                    ));
                    let notify = list
                        .iter()
                        .find(|d| d.hardware_id.eq_ignore_ascii_case(&change.hardware_id))
                        .map(|d| {
                            let status = if d.is_connected {
                                if d.is_attached {
                                    lang::t("ConnectedToWsl")
                                } else {
                                    lang::t("ConnectedToWindows")
                                }
                            } else {
                                lang::t("Disconnected").to_owned()
                            };
                            format!("\"{}({})\" {status}.", d.short_name(), d.hardware_id)
                        });
                    let _ = tx.send(UiMsg::UsbNotify(notify));
                    let _ = tx.send(UiMsg::Lists(list));
                    let _ = tx.send(UiMsg::UsbListFinished);
                    egui_ctx.request_repaint();
                }
                Err(e) => {
                    log::warn(&format!("Failed to refresh after USB change: {e}"));
                    let _ = tx.send(UiMsg::UsbListFinished);
                    egui_ctx.request_repaint();
                }
            })
            .expect("spawn usb event thread");
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    pub fn open_settings(&mut self) {
        self.settings_draft = self.cfg.app_config.clone();
        self.netcards = crate::sys::network_cards();
        self.show_settings = true;
    }

    pub fn close_settings(&mut self) {
        self.show_settings = false;
    }

    pub fn settings_ok(&mut self) {
        self.cfg.app_config = self.settings_draft.clone();
        if self.cfg.app_config.lang.is_empty() {
            self.cfg.app_config.lang = if lang::is_zh() { "zh" } else { "en" }.to_owned();
        }
        self.save_cfg();
        self.show_settings = false;
    }

    pub fn reset_config(&mut self) {
        self.cfg.reset();
        self.save_cfg();
        self.settings_draft = self.cfg.app_config.clone();
        self.toast_info(&lang::t("ConfigReset"));
    }

    pub fn clear_logs(&mut self) {
        crate::log::clear_history();
        self.dialog = Some(Dialog {
            title: lang::t("Note"),
            message: lang::t("HistoryLogsCleared"),
        });
    }

    pub fn open_config_dir(&mut self) {
        crate::log::open_dir();
    }

    // ------------------------------------------------------------------
    // Getters used by the UI
    // ------------------------------------------------------------------

    pub fn devices(&self) -> &[DevRow] {
        &self.device_rows
    }

    pub fn persisted(&self) -> &[UsbDevice] {
        &self.persisted_rows
    }

    pub fn auto_devices(&self) -> &[UsbDevice] {
        &self.auto_rows
    }

    pub fn cfg(&self) -> &SystemConfig {
        &self.cfg
    }

    pub fn cfg_mut(&mut self) -> &mut SystemConfig {
        &mut self.cfg
    }

    pub fn selected_hwid(&self, tab: Tab) -> Option<String> {
        self.selected_hwid.get(&tab).cloned()
    }

    pub fn set_selected(&mut self, tab: Tab, hwid: String) {
        self.selected_hwid.insert(tab, hwid);
    }

    pub fn show_filtered(&self) -> bool {
        self.show_filtered
    }

    pub fn set_show_filtered(&mut self, v: bool) {
        self.show_filtered = v;
        self.restore_selections();
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn toast_mut(&mut self) -> &mut Vec<Toast> {
        &mut self.toasts
    }

    pub fn dialog(&self) -> &Option<Dialog> {
        &self.dialog
    }

    pub fn dialog_mut(&mut self) -> &mut Option<Dialog> {
        &mut self.dialog
    }

    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.cfg.app_config.dark_mode = !self.cfg.app_config.dark_mode;
        self.save_cfg();
        self.apply_theme(ctx);
    }

    pub fn set_lang(&mut self, zh: bool, ctx: &egui::Context) {
        self.cfg.app_config.lang = if zh { "zh" } else { "en" }.to_owned();
        self.save_cfg();
        self.apply_theme(ctx);
        self.update_tray_language();
    }

    /// 托盘弹出菜单的文字跟随当前语言更新。
    pub fn update_tray_language(&mut self) {
        if let (Some(show), Some(exit)) = (&self.tray_show, &self.tray_exit) {
            show.set_text(lang::t("Show"));
            exit.set_text(lang::t("Exit"));
        }
    }

    pub fn set_settings_draft(&mut self, draft: AppConfig) {
        self.settings_draft = draft;
    }

    pub fn settings_draft(&self) -> &AppConfig {
        &self.settings_draft
    }

    pub fn update_settings_draft(&mut self, f: impl FnOnce(&mut AppConfig)) {
        f(&mut self.settings_draft);
    }

    pub fn settings_visible(&self) -> bool {
        self.show_settings
    }

    pub fn netcards(&self) -> &[(String, String)] {
        &self.netcards
    }

    pub fn hidden_count(&self) -> usize {
        self.device_rows.iter().filter(|r| r.is_filtered).count()
    }

    pub fn close_to_tray_requested(&self) -> bool {
        self.cfg.app_config.close_to_tray && !self.exiting && self.tray.is_some()
    }

    pub fn mark_hidden(&mut self) {
        self.hidden = true;
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    pub fn current_device(&self) -> Option<&UsbDevice> {
        let hwid = self
            .selected_hwid
            .get(&self.tab)
            .cloned()
            .unwrap_or_default();
        match self.tab {
            Tab::Devices => self
                .device_rows
                .iter()
                .find(|r| r.dev.hardware_id == hwid)
                .map(|r| &r.dev),
            Tab::Persisted => self.persisted_rows.iter().find(|d| d.hardware_id == hwid),
            Tab::AutoAttach => self.auto_rows.iter().find(|d| d.hardware_id == hwid),
        }
    }

    pub fn row_flags(&self, row: &DevRow) -> RowFlags {
        RowFlags {
            auto_enabled: !row.is_filtered,
            bind_enabled: !row.is_filtered && (!row.is_auto || !row.dev.is_bound),
            attach_enabled: !row.is_filtered
                && if row.is_auto {
                    !row.dev.is_attached
                } else {
                    row.dev.is_bound
                },
            force_enabled: !row.is_filtered && !row.is_auto && !row.dev.is_bound,
        }
    }

    pub fn stop_all(&mut self) {
        if let Ok(mut dm) = self.daemons.lock() {
            dm.stop_all();
        }
        self.monitor.take();
        self.tray.take();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RowFlags {
    pub auto_enabled: bool,
    pub bind_enabled: bool,
    pub attach_enabled: bool,
    pub force_enabled: bool,
}

// ---------------------------------------------------------------------------
// Icon and tray helpers
// ---------------------------------------------------------------------------

pub fn load_window_icon() -> Option<egui::IconData> {
    let png = include_bytes!("../assets/appicon.png");
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let img = img
        .resize(64, 64, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    })
}

fn tray_icon() -> Option<tray_icon::Icon> {
    let png = include_bytes!("../assets/appicon.png");
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let img = img
        .resize(32, 32, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()
}

fn build_tray(
    tx: Sender<UiMsg>,
    egui_ctx: egui::Context,
) -> Result<
    Option<(
        tray_icon::TrayIcon,
        tray_icon::menu::MenuItem,
        tray_icon::menu::MenuItem,
    )>,
    String,
> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

    let Some(icon) = tray_icon() else {
        return Ok(None);
    };
    let menu = Menu::new();
    let show = MenuItem::with_id("show", lang::t("Show"), true, None);
    let exit = MenuItem::with_id("exit", lang::t("Exit"), true, None);
    menu.append_items(&[&show, &exit])
        .map_err(|e| e.to_string())?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("USBIP Device Manager")
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;

    let tx2 = tx.clone();
    let ctx2 = egui_ctx.clone();
    std::thread::Builder::new()
        .name("tray-menu".to_owned())
        .spawn(move || {
            let rx = MenuEvent::receiver();
            while let Ok(event) = rx.recv() {
                match event.id.0.as_str() {
                    "show" => {
                        let _ = tx2.send(UiMsg::ShowWindow);
                        ctx2.request_repaint();
                    }
                    "exit" => {
                        let _ = tx2.send(UiMsg::ExitApp);
                        ctx2.request_repaint();
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let tx3 = tx;
    let ctx3 = egui_ctx;
    std::thread::Builder::new()
        .name("tray-events".to_owned())
        .spawn(move || {
            let rx = TrayIconEvent::receiver();
            while let Ok(event) = rx.recv() {
                if let TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } = event
                {
                    let _ = tx3.send(UiMsg::ShowWindow);
                    ctx3.request_repaint();
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(Some((tray, show, exit)))
}
