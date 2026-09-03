// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Detects USB plug/unplug by polling SetupAPI and emits `VID:PID` changes.

use std::collections::HashSet;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::sys;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbChange {
    pub hardware_id: String,
    pub connected: bool,
}

pub struct UsbMonitor {
    handle: Option<JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl UsbMonitor {
    pub fn start<F>(cb: F) -> Self
    where
        F: Fn(UsbChange) + Send + 'static,
    {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("usb-monitor".to_owned())
            .spawn(move || {
                let mut previous: HashSet<String> = sys::usb_hardware_ids().into_iter().collect();
                while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    let current: HashSet<String> = sys::usb_hardware_ids().into_iter().collect();
                    for id in current.difference(&previous) {
                        cb(UsbChange {
                            hardware_id: id.clone(),
                            connected: true,
                        });
                    }
                    for id in previous.difference(&current) {
                        cb(UsbChange {
                            hardware_id: id.clone(),
                            connected: false,
                        });
                    }
                    previous = current;
                }
            })
            .expect("failed to start usb monitor thread");
        Self {
            handle: Some(handle),
            stop,
        }
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for UsbMonitor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
