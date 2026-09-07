// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

#![allow(dead_code)]

//! Application state, background workers and action dispatch.

use crate::config::{self, AppConfig, SystemConfig, UsbDevice};
use crate::lang;
use crate::log;
use crate::monitor::UsbMonitor;
use crate::usbipd::{self, DaemonManager, Usbipd};
use eframe::egui;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// USB 广播事件触发 usbipd state 查询的去抖 / 补查参数。
/// 广播风暴后等待枚举稳定再查询的冷却时间。
const USB_EVENT_COOLDOWN: Duration = Duration::from_millis(900);
/// 查询后 state 未变化的自动补查间隔。
const USB_RECHECK_INTERVAL: Duration = Duration::from_millis(600);
/// 单次广播触发后最多自动补查次数（state 相对广播有短暂滞后）。
const USB_RECHECK_LIMIT: u8 = 2;
/// 附加过程中的宽限期，期间不因 state 差集停止守护进程。
const ATTACH_GUARD_WINDOW: Duration = Duration::from_secs(15);
/// auto 设备插入后未附加时的兜底检查间隔（静默查询 usbipd state）。
const AUTO_WATCH_INTERVAL: Duration = Duration::from_millis(200);
/// 兜底检查总轮数：每轮查询一次，未附加则执行一次附加。
const AUTO_WATCH_ROUNDS: u8 = 3;

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
    ListFailed(String),
    OpDone { ok: bool, message: String },
    UsbActivity,
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
    /// 自动附加被“8 秒防抖”推迟后，下一次重新评估的时间。
    auto_retry_at: Option<Instant>,
    /// auto 设备插入后未附加的兜底检查剩余轮数（0 = 未武装）。
    auto_watch_rounds: u8,
    /// 3 轮兜底是否已用完（等待下一次状态变化/插拔事件重新武装）。
    auto_watch_exhausted: bool,
    /// 兜底观察下一次检查的时间点（200ms 节流）。
    auto_watch_next: Option<Instant>,
    /// 静默状态查询是否已在途（不置 busy，避免 spinner 闪烁）。
    quiet_poll_inflight: bool,
    /// 收到 USB 广播后待处理的“查询一次 usbipd state”标记（去抖合并）。
    usb_activity_pending: bool,
    /// 广播后 state 尚未反映变化时的自动补查剩余次数。
    usb_rechecks: u8,
    /// 最近一次由 USB 广播发起的 state 查询时间（风暴冷却用）。
    last_usb_refresh: Option<Instant>,
    /// 上一次完整 usbipd state 快照，用于差集判断。
    state_snapshot: Option<Vec<UsbDevice>>,

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
            auto_retry_at: None,
            auto_watch_rounds: 0,
            auto_watch_exhausted: false,
            auto_watch_next: None,
            quiet_poll_inflight: false,
            usb_activity_pending: false,
            usb_rechecks: 0,
            last_usb_refresh: None,
            state_snapshot: None,
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

        let tx_usb = app.tx.clone();
        let ctx_usb = cc.egui_ctx.clone();
        app.monitor = Some(UsbMonitor::start(move || {
            let _ = tx_usb.send(UiMsg::UsbActivity);
            ctx_usb.request_repaint();
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
                    self.quiet_poll_inflight = false;
                    self.apply_state_snapshot(devices, ctx);
                }
                UiMsg::ListFailed(e) => {
                    self.busy = false;
                    self.quiet_poll_inflight = false;
                    log::warn(&format!("usbipd state fetch failed: {e}"));
                    if self.initialized {
                        // 只提示，绝不清空当前列表，避免瞬时失败误杀守护进程。
                        self.toasts.push(Toast {
                            kind: ToastKind::Error,
                            text: e,
                            expires: Instant::now() + Duration::from_secs(6),
                        });
                    } else {
                        self.dialog = Some(Dialog {
                            title: lang::t("Error"),
                            message: e,
                        });
                    }
                    ctx.request_repaint();
                }
                UiMsg::UsbActivity => {
                    self.queue_usb_activity(ctx);
                }
                UiMsg::OpDone { ok, message } => {
                    self.busy = false;
                    log::info(&format!(
                        "operation finished: ok={ok} message={}",
                        message.trim()
                    ));
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
        let hidden = self.device_rows.iter().filter(|r| r.is_filtered).count();
        log::info(&format!(
            "list applied: connected={} persisted={} auto_list={} hidden={}",
            self.device_rows.len(),
            self.persisted_rows.len(),
            self.auto_rows.len(),
            hidden
        ));
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
        self.maybe_auto_attach_inner(false);
    }

    /// 兜底附加：无视 8 秒防抖，供“200ms × 3 轮”兜底观察使用。
    fn maybe_auto_attach_forced(&mut self) {
        self.maybe_auto_attach_inner(true);
    }

    fn maybe_auto_attach_inner(&mut self, ignore_debounce: bool) {
        if !self.initialized || self.busy {
            return;
        }
        let now = Instant::now();
        let mut candidate: Option<UsbDevice> = None;
        let mut retry_earliest: Option<Instant> = None;
        for row in &self.device_rows {
            if !row.is_auto || !row.dev.is_connected || row.dev.is_attached {
                continue;
            }
            let id = if self.cfg.app_config.use_bus_id {
                row.dev.bus_id.clone()
            } else {
                row.dev.hardware_id.clone()
            };
            let recent = self
                .pending_attach
                .get(&row.dev.hardware_id)
                .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(8));
            if recent && !ignore_debounce {
                log::info(&format!("auto attach deferred (debounce): id={id}"));
                let expires = self
                    .pending_attach
                    .get(&row.dev.hardware_id)
                    .map(|t| *t + Duration::from_secs(8))
                    .unwrap_or(now);
                retry_earliest = Some(
                    retry_earliest
                        .map(|earliest| earliest.min(expires))
                        .unwrap_or(expires),
                );
                continue;
            }
            if ignore_debounce && recent {
                log::info(&format!(
                    "auto attach watchdog: retrying despite debounce: id={id}"
                ));
            }
            // 守护进程还活着但设备未附加：说明 --auto-attach 守护进程卡住了
            // （手动 usbipd attach 能成功即是佐证）。停掉它，改由本应用接管，
            // 否则它会被误判成“附加中”而永远不再触发附加。
            let attaching = self
                .daemons
                .lock()
                .map(|mut dm| dm.attaching(&id))
                .unwrap_or(false);
            if attaching {
                log::warn(&format!(
                    "auto attach: stale daemon alive while device unattached, stopping it: id={id}"
                ));
                if let Ok(mut dm) = self.daemons.lock() {
                    dm.stop_matching(&id);
                    if !row.dev.hardware_id.is_empty() && id != row.dev.hardware_id {
                        dm.stop_matching(&row.dev.hardware_id);
                    }
                }
            }
            candidate = Some(row.dev.clone());
            break;
        }
        if let Some(dev) = candidate {
            log::info(&format!(
                "auto attach trigger: id={} hardware_id={}",
                if self.cfg.app_config.use_bus_id {
                    dev.bus_id.clone()
                } else {
                    dev.hardware_id.clone()
                },
                dev.hardware_id
            ));
            self.pending_attach.insert(dev.hardware_id.clone(), now);
            self.attach(dev, true);
        }
        // 无论本轮是否触发附加，都保留其它设备“防抖到期后的重试”计划。
        self.auto_retry_at = retry_earliest;
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
                    let _ = tx.send(UiMsg::ListFailed(e));
                }
            })
            .expect("spawn refresh thread");
    }

    /// 静默状态查询：不置 `busy`/spinner，结果只在真正变化时更新 UI。
    /// 供 auto 附加兜底观察使用。
    fn start_quiet_state_poll(&mut self) {
        if self.quiet_poll_inflight {
            return;
        }
        let Some(client) = self.usbipd.clone() else {
            return;
        };
        self.quiet_poll_inflight = true;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("usbipd-state-poll".to_owned())
            .spawn(move || match client.list_devices() {
                Ok(list) => {
                    let _ = tx.send(UiMsg::Lists(list));
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::ListFailed(e));
                }
            })
            .expect("spawn quiet state poll thread");
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

    /// USB 广播只是“敲门砖”：收到后合并为一次状态查询请求。真正的插拔
    /// 结论以 usbipd state 快照差集为准（见 [`App::apply_state_snapshot`]）。
    pub fn queue_usb_activity(&mut self, ctx: &egui::Context) {
        log::info("USB activity signal received: scheduling usbipd state check");
        self.usb_activity_pending = true;
        self.usb_rechecks = USB_RECHECK_LIMIT;
        ctx.request_repaint_after(Duration::from_millis(120));
    }

    /// 每帧处理一次“需要检查 usbipd state”的请求（广播触发或自动补查）。
    pub fn flush_usb_activity(&mut self, ctx: &egui::Context) {
        if self.busy {
            if self.usb_activity_pending || self.usb_rechecks > 0 {
                // 绑定/附加等操作进行中，稍后重试。
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        let now = Instant::now();
        if self.usb_activity_pending {
            if self
                .last_usb_refresh
                .is_some_and(|t| now.duration_since(t) < USB_EVENT_COOLDOWN)
            {
                // 广播风暴还没安静下来：等系统枚举稳定后再查一次。
                ctx.request_repaint_after(USB_EVENT_COOLDOWN);
                return;
            }
            self.usb_activity_pending = false;
            log::info("USB activity -> usbipd state check");
            self.last_usb_refresh = Some(now);
            self.start_refresh();
            return;
        }
        if self.usb_rechecks > 0 {
            if self
                .last_usb_refresh
                .is_some_and(|t| now.duration_since(t) < USB_RECHECK_INTERVAL)
            {
                ctx.request_repaint_after(USB_RECHECK_INTERVAL);
                return;
            }
            log::info(&format!(
                "USB state recheck (remaining={})",
                self.usb_rechecks
            ));
            self.last_usb_refresh = Some(now);
            self.start_refresh();
        }
    }

    /// 广播只负责“提醒”，增删改一律以 usbipd state 快照差集为准：
    /// - 快照无变化 → 不重建列表、不重绘（消除附加成功后的持续刷新）。
    /// - 设备从快照消失且上一帧处于 attached → 结束其自动附加守护进程。
    /// - 新设备出现 → 提示用户，并由 [`App::apply_lists`] 触发自动附加。
    fn apply_state_snapshot(&mut self, devices: Vec<UsbDevice>, ctx: &egui::Context) {
        let first = self.state_snapshot.is_none();
        if !first {
            let mut prev = self.state_snapshot.clone().unwrap_or_default();
            let mut cur = devices.clone();
            sort_by_instance(&mut prev);
            sort_by_instance(&mut cur);
            if prev == cur {
                if self.usb_activity_pending {
                    log::info(&format!(
                        "usbipd state unchanged ({} device(s)), activity pending",
                        devices.len()
                    ));
                    // 冷却结束后由 flush_usb_activity 再查一次。
                    ctx.request_repaint_after(USB_EVENT_COOLDOWN);
                } else if self.usb_rechecks > 0 {
                    self.usb_rechecks -= 1;
                    log::info(&format!(
                        "usbipd state unchanged ({} device(s)), rechecks left={}",
                        devices.len(),
                        self.usb_rechecks
                    ));
                    if self.usb_rechecks > 0 {
                        ctx.request_repaint_after(USB_RECHECK_INTERVAL);
                    }
                }
                // 兜底轮询（无活动、无补查）时静默：无变化就不打扰。
                return;
            }
        }

        let previous = self.state_snapshot.clone().unwrap_or_default();
        if !first {
            let (removed, added) = diff_snapshots(&previous, &devices);
            for dev in &removed {
                log::info(&format!(
                    "usbipd state lost device: instance={} hwid={} was_attached={}",
                    dev.instance_id, dev.hardware_id, dev.is_attached
                ));
                if dev.is_attached {
                    self.stop_daemons_for_removed(dev);
                }
            }
            for dev in &added {
                log::info(&format!(
                    "usbipd state gained device: instance={} hwid={}",
                    dev.instance_id, dev.hardware_id
                ));
            }
            self.push_change_toasts(&removed, &added);
        }
        self.usb_rechecks = 0;
        // 状态发生变化 = 新的插拔/刷新事件：允许兜底观察重新武装。
        self.auto_watch_exhausted = false;
        self.state_snapshot = Some(devices.clone());
        log::info(&format!(
            "usbipd state changed: {} -> {} device(s)",
            previous.len(),
            devices.len()
        ));
        self.apply_lists(devices);
        ctx.request_repaint();
    }

    /// 设备已从 usbipd state 消失（物理拔出）。若它此前 attached，说明其
    /// 自动附加守护进程应随之退出；附加中/刚附加过则跳过，避免中途误杀。
    fn stop_daemons_for_removed(&mut self, dev: &UsbDevice) {
        let now = Instant::now();
        let recent = self
            .pending_attach
            .get(&dev.hardware_id)
            .is_some_and(|t| now.duration_since(*t) < ATTACH_GUARD_WINDOW);
        if recent {
            log::info(&format!(
                "device removal during attach window, daemon kept: hwid={}",
                dev.hardware_id
            ));
            return;
        }
        let id = if self.cfg.app_config.use_bus_id {
            &dev.bus_id
        } else {
            &dev.hardware_id
        };
        let attaching = self
            .daemons
            .lock()
            .map(|mut dm| dm.attaching(id))
            .unwrap_or(false);
        if attaching {
            log::info(&format!(
                "device removal while attaching, daemon kept: id={id}"
            ));
            return;
        }
        let mut needles: Vec<String> = Vec::new();
        if !dev.bus_id.is_empty() {
            needles.push(dev.bus_id.clone());
        }
        if !dev.hardware_id.is_empty() {
            needles.push(dev.hardware_id.clone());
        }
        if let Ok(mut dm) = self.daemons.lock() {
            for needle in &needles {
                dm.stop_matching(needle);
            }
        }
        log::info(&format!(
            "attached device removed, auto-attach daemon stopped: hwid={} bus={}",
            dev.hardware_id, dev.bus_id
        ));
    }

    fn push_change_toasts(&mut self, removed: &[&UsbDevice], added: &[&UsbDevice]) {
        for dev in added {
            if !dev.is_connected {
                continue;
            }
            let status = if dev.is_attached {
                lang::t("ConnectedToWsl")
            } else {
                lang::t("ConnectedToWindows")
            };
            let text = format!("\"{}({})\" {status}.", dev.short_name(), dev.hardware_id);
            self.toasts.push(Toast {
                kind: ToastKind::Info,
                text,
                expires: Instant::now() + Duration::from_secs(5),
            });
        }
        for dev in removed {
            let text = format!(
                "\"{}({})\" {}.",
                dev.short_name(),
                dev.hardware_id,
                lang::t("Disconnected")
            );
            self.toasts.push(Toast {
                kind: ToastKind::Info,
                text,
                expires: Instant::now() + Duration::from_secs(5),
            });
        }
    }

    /// 自动附加被防抖推迟后的定时重试：到期时触发一次状态刷新，
    /// 由 apply_lists -> maybe_auto_attach 完成真正的重试。
    pub(crate) fn check_auto_retry(&mut self, ctx: &egui::Context) {
        let Some(deadline) = self.auto_retry_at else {
            return;
        };
        if Instant::now() >= deadline {
            log::info("auto attach debounce expired, re-evaluating");
            self.auto_retry_at = None;
            self.need_refresh = true;
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(deadline - Instant::now());
        }
    }

    /// 兜底附加：auto 设备已插入（connected && !attached）但尚未附加时，
    /// 每 200ms 静默查询一次 usbipd state 并执行一次附加，共 3 轮；一旦
    /// 附加成功或设备消失即停止。用完 3 轮后不再自动重试，等待下一次
    /// 状态变化（如插拔广播触发刷新）重新武装。全程不占用 busy/spinner。
    pub(crate) fn watch_auto_attach(&mut self, ctx: &egui::Context) {
        if !self.initialized || self.busy {
            return;
        }
        let now = Instant::now();
        let unattached = |r: &DevRow| r.is_auto && r.dev.is_connected && !r.dev.is_attached;
        if !self.device_rows.iter().any(unattached) {
            self.auto_watch_rounds = 0;
            self.auto_watch_next = None;
            self.auto_watch_exhausted = false;
            return;
        }
        if self.auto_watch_exhausted {
            // 3 轮已用完：等待下一次状态变化/插拔事件再武装。
            return;
        }
        if self.auto_watch_rounds == 0 {
            log::info("auto attach watchdog armed: 200 ms x 3 rounds");
            self.auto_watch_rounds = AUTO_WATCH_ROUNDS;
            self.auto_watch_next = None;
        }
        ctx.request_repaint_after(AUTO_WATCH_INTERVAL);
        if self.auto_watch_next.is_some_and(|t| now < t) {
            return;
        }
        self.auto_watch_next = Some(now + AUTO_WATCH_INTERVAL);
        self.auto_watch_rounds -= 1;
        log::info(&format!(
            "auto attach watchdog round {}/{}",
            AUTO_WATCH_ROUNDS - self.auto_watch_rounds,
            AUTO_WATCH_ROUNDS
        ));
        // 静默查询一次当前状态：若 usbipd 守护进程其实已完成附加，diff 会
        // 更新快照，下个周期检测到已附加就会停止。
        self.start_quiet_state_poll();
        // 未附加则执行一次附加（无视 8 秒防抖）；卡死的 --auto-attach
        // 守护进程会在 maybe_auto_attach_forced 内被清理。
        self.maybe_auto_attach_forced();
        if self.auto_watch_rounds == 0 {
            self.auto_watch_exhausted = true;
            log::warn("auto attach watchdog finished 3 rounds; device still not attached");
        }
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

// ---------------------------------------------------------------------------
// usbipd state snapshot diff helpers
// ---------------------------------------------------------------------------

/// 按 InstanceId 排序，使两次快照可直接用 `PartialEq` 比较顺序无关状态。
fn sort_by_instance(list: &mut Vec<UsbDevice>) {
    list.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
}

/// 按 InstanceId（不区分大小写）计算真正消失/新增的设备。
fn diff_snapshots<'a>(
    prev: &'a [UsbDevice],
    cur: &'a [UsbDevice],
) -> (Vec<&'a UsbDevice>, Vec<&'a UsbDevice>) {
    let removed = prev
        .iter()
        .filter(|d| {
            !cur.iter()
                .any(|c| c.instance_id.eq_ignore_ascii_case(&d.instance_id))
        })
        .collect();
    let added = cur
        .iter()
        .filter(|d| {
            !prev
                .iter()
                .any(|p| p.instance_id.eq_ignore_ascii_case(&d.instance_id))
        })
        .collect();
    (removed, added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(instance_id: &str) -> UsbDevice {
        UsbDevice {
            instance_id: instance_id.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn diff_detects_removed_and_added_by_instance_id() {
        let prev = vec![
            dev("USB\\VID_0403&PID_6001\\A50285BI"),
            dev("USB\\VID_1234&PID_5678\\STAYS"),
        ];
        let cur = vec![
            dev("USB\\VID_1234&PID_5678\\stays"),
            dev("USB\\VID_8087&PID_0A2B\\NEWONE"),
        ];
        let (removed, added) = diff_snapshots(&prev, &cur);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].instance_id, "USB\\VID_0403&PID_6001\\A50285BI");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].instance_id, "USB\\VID_8087&PID_0A2B\\NEWONE");
    }

    #[test]
    fn same_state_in_any_order_is_not_a_change() {
        let mut a = vec![
            dev("USB\\VID_0403&PID_6001\\A50285BI"),
            dev("USB\\VID_1234&PID_5678\\STAYS"),
        ];
        let mut b = vec![
            dev("USB\\VID_1234&PID_5678\\STAYS"),
            dev("USB\\VID_0403&PID_6001\\A50285BI"),
        ];
        sort_by_instance(&mut a);
        sort_by_instance(&mut b);
        assert_eq!(a, b);
    }
}
