// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Detects USB plug/unplug through `RegisterDeviceNotification`
//! (`WM_DEVICECHANGE`) on a hidden message-only window. The thread blocks
//! in `GetMessageW` and only enumerates devices after a system notification,
//! so there is no periodic polling.

use std::collections::HashSet;
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
    DispatchMessageW, GetMessageW, HWND_MESSAGE, PostThreadMessageW, RegisterClassW,
    RegisterDeviceNotificationW, TranslateMessage, UnregisterDeviceNotification, WM_DEVICECHANGE,
    WM_QUIT, WNDCLASSW,
};
use windows_sys::core::{GUID, PCWSTR};

use crate::log;
use crate::sys;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbChange {
    pub instance_id: String,
    pub connected: bool,
}

const WINDOW_CLASS: &str = "usbipdm-usb-notify";

/// GUID_DEVINTERFACE_USB_DEVICE:
/// {A5DCBF10-6530-11D2-901F-00C04FB951ED}
const USB_DEVICE_CLASS_GUID: GUID = GUID {
    data1: 0xA5DC_BF10,
    data2: 0x6530,
    data3: 0x11D2,
    data4: [0x90, 0x1F, 0x00, 0xC0, 0x4F, 0xB9, 0x51, 0xED],
};

pub struct UsbMonitor {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    thread_id: Arc<Mutex<Option<u32>>>,
}

impl UsbMonitor {
    pub fn start<F>(cb: F) -> Self
    where
        F: Fn(UsbChange) + Send + 'static,
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
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn emit_diff<F>(previous: &mut HashSet<String>, cb: &F)
where
    F: Fn(UsbChange),
{
    let current: HashSet<String> = sys::usb_instance_ids().into_iter().collect();
    for id in current.difference(previous) {
        log::info(&format!("USB device plugged: {id}"));
        cb(UsbChange {
            instance_id: id.clone(),
            connected: true,
        });
    }
    for id in previous.difference(&current) {
        log::info(&format!("USB device unplugged: {id}"));
        cb(UsbChange {
            instance_id: id.clone(),
            connected: false,
        });
    }
    *previous = current;
}

fn monitor_loop<F>(cb: F, stop: Arc<AtomicBool>, thread_id: Arc<Mutex<Option<u32>>>)
where
    F: Fn(UsbChange) + Send + 'static,
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

        let hwnd = CreateWindowExW(
            0,
            class_ptr,
            wide("usbipdm-usb-notify").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log::error("failed to create USB notification window");
            return;
        }

        let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = std::mem::zeroed();
        filter.dbcc_size = size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
        filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        filter.dbcc_classguid = USB_DEVICE_CLASS_GUID;
        let notify = RegisterDeviceNotificationW(
            hwnd,
            &filter as *const _ as *const core::ffi::c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        if notify.is_null() {
            log::warn("RegisterDeviceNotification failed; USB events will not be received");
        }

        if let Ok(mut tid) = thread_id.lock() {
            *tid = Some(GetCurrentThreadId());
        }
        log::info("USB notification window ready (event-driven)");

        let mut previous: HashSet<String> = sys::usb_instance_ids().into_iter().collect();
        let mut msg: windows_sys::Win32::UI::WindowsAndMessaging::MSG = std::mem::zeroed();

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r <= 0 {
                break;
            }
            if msg.message == WM_DEVICECHANGE {
                let wparam = msg.wParam as u32;
                if wparam == DBT_DEVICEARRIVAL || wparam == DBT_DEVICEREMOVECOMPLETE {
                    log::info(&format!(
                        "USB device notification received: wparam=0x{wparam:X}"
                    ));
                    emit_diff(&mut previous, &cb);
                    // 系统通知到达时设备列表可能尚未更新，400ms 后补查一次。
                    std::thread::sleep(Duration::from_millis(400));
                    emit_diff(&mut previous, &cb);
                }
                continue;
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
