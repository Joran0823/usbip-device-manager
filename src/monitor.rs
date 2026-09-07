// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Windows USB change broadcast watcher.
//!
//! A hidden top-level window receives `WM_DEVICECHANGE` notifications
//! (`DBT_DEVICEARRIVAL`, `DBT_DEVICEREMOVECOMPLETE` and the `DBT_DEVNODES_CHANGED`
//! broadcast) through `RegisterDeviceNotification`. These broadcasts are only
//! used as a *trigger*: the watcher never enumerates or identifies a device
//! itself, because Windows PnP noise (attach side effects, node swaps, dense
//! bus broadcasts) cannot be reliably mapped to a physical plug/unplug.
//!
//! Bursts of events are coalesced with a short timer; when the traffic settles
//! the watcher signals the UI thread ("something USB-related changed"), which
//! then refreshes `usbipd state` and acts on the real device diff.

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVTYP_DEVICEINTERFACE,
    DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GetMessageW, KillTimer, PostThreadMessageW, RegisterClassW,
    RegisterDeviceNotificationW, SetTimer, TranslateMessage, UnregisterDeviceNotification,
    WM_DEVICECHANGE, WM_QUIT, WM_TIMER, WNDCLASSW, WS_POPUP,
};
use windows_sys::core::{GUID, PCWSTR};

use crate::log;

/// `DBT_DEVNODES_CHANGED` is broadcast to all top-level windows for PnP node
/// changes. It is noisy, but harmless here because it only schedules a state
/// check whose result is authoritative.
const DBT_DEVNODES_CHANGED: u32 = 0x0007;

const WINDOW_CLASS: &str = "usbipdm-usb-notify";

/// `dbcc_classguid = GUID_NULL`: receive notifications for all device
/// interface classes. Device identity is never read from the notification.
const ALL_CLASSES_GUID: GUID = GUID {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0; 8],
};

/// Coalescing delay: a physical insert/remove emits several broadcasts, and
/// they are collapsed into a single signal once traffic settles.
const COALESCE_MS: u32 = 200;
const COALESCE_TIMER_ID: usize = 1;

struct MonitorCtx {
    /// `true` while a coalescing timer is pending.
    pending: bool,
    cb: Box<dyn Fn() + Send>,
}

thread_local! {
    static CTX: RefCell<Option<MonitorCtx>> = const { RefCell::new(None) };
}

pub struct UsbMonitor {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    thread_id: Arc<Mutex<Option<u32>>>,
}

impl UsbMonitor {
    pub fn start<F>(cb: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_id: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let stop2 = stop.clone();
        let thread_id2 = thread_id.clone();
        let handle = std::thread::Builder::new()
            .name("usb-monitor".to_owned())
            .spawn(move || monitor_loop(cb, stop2, thread_id2))
            .expect("failed to start usb monitor thread");
        Self {
            handle: Some(handle),
            stop,
            thread_id,
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for _ in 0..200 {
            let id = *self.thread_id.lock().unwrap();
            if let Some(id) = id {
                unsafe {
                    let _ = PostThreadMessageW(id, WM_QUIT, 0, 0);
                }
                break;
            }
            if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for UsbMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    if msg == WM_DEVICECHANGE {
        let w = wparam as u32;
        if w == DBT_DEVICEARRIVAL || w == DBT_DEVICEREMOVECOMPLETE || w == DBT_DEVNODES_CHANGED {
            log::info(&format!("WM_DEVICECHANGE (sent) wparam=0x{w:X}"));
            // 只记录“有变化发生”，不做任何枚举或设备识别；200ms 后合并为一个信号。
            CTX.with(|ctx| {
                if let Ok(mut guard) = ctx.try_borrow_mut() {
                    if let Some(c) = guard.as_mut() {
                        if !c.pending {
                            c.pending = true;
                            unsafe {
                                let _ = SetTimer(hwnd, COALESCE_TIMER_ID, COALESCE_MS, None);
                            }
                            log::info("USB broadcast burst: state check scheduled");
                        }
                    }
                }
            });
            return 1;
        }
    }
    if msg == WM_TIMER && wparam == COALESCE_TIMER_ID {
        unsafe {
            let _ = KillTimer(hwnd, COALESCE_TIMER_ID);
        }
        CTX.with(|ctx| {
            if let Ok(mut guard) = ctx.try_borrow_mut() {
                if let Some(c) = guard.as_mut() {
                    c.pending = false;
                    (c.cb)();
                }
            }
        });
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn monitor_loop<F>(cb: F, stop: Arc<AtomicBool>, thread_id: Arc<Mutex<Option<u32>>>)
where
    F: Fn() + Send + 'static,
{
    let class_name = wide(WINDOW_CLASS);
    let class_ptr: PCWSTR = class_name.as_ptr();

    let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
    wc.lpfnWndProc = Some(wnd_proc);
    wc.lpszClassName = class_ptr;

    unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        wc.hInstance = hinstance;

        if RegisterClassW(&wc) == 0 {
            log::error("failed to register USB notification window class");
            return;
        }

        // 隐藏的普通顶层窗口（而非 message-only 窗口）接收设备通知。
        let hwnd = CreateWindowExW(
            0,
            class_ptr,
            wide("usbipdm-usb-notify").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log::error("failed to create USB notification window");
            return;
        }

        CTX.with(|ctx| {
            *ctx.borrow_mut() = Some(MonitorCtx {
                pending: false,
                cb: Box::new(cb),
            });
        });

        let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = std::mem::zeroed();
        filter.dbcc_size = size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
        filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        filter.dbcc_classguid = ALL_CLASSES_GUID;
        let notify = RegisterDeviceNotificationW(
            hwnd,
            &filter as *const _ as *const core::ffi::c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        if notify.is_null() {
            log::warn("RegisterDeviceNotification failed; USB events will not be received");
        } else {
            log::info("device notifications registered");
        }

        if let Ok(mut tid) = thread_id.lock() {
            *tid = Some(GetCurrentThreadId());
        }
        log::info("USB notification window ready (broadcast -> usbipd state check)");

        let mut msg: windows_sys::Win32::UI::WindowsAndMessaging::MSG = std::mem::zeroed();
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if !notify.is_null() {
            let _ = UnregisterDeviceNotification(notify);
        }
        let _ = DestroyWindow(hwnd);
        log::info("USB notification window stopped");
    }
}
