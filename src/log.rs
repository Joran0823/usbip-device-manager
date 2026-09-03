// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Minimal file logger writing to `%APPDATA%\WSL USB Manager\Logs\yyyyMMdd.log`.

use crate::config;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOCK: Mutex<()> = Mutex::new(());

fn logs_dir() -> PathBuf {
    config::app_data_dir().join("Logs")
}

fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // civil date from epoch (days since 1970-01-01, Howard Hinnant's algorithm)
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, _m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

fn today_file() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}.log")
}

fn write_line(level: &str, msg: &str) {
    let _g = LOCK.lock().unwrap();
    let dir = logs_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(today_file());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let tid = std::thread::current().name().unwrap_or("?").to_owned();
        let line = format!("{} [{}] {:<5} {}\r\n", now(), tid, level, msg);
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn init() {
    let _ = fs::create_dir_all(logs_dir());
    info("Starting USBIP Device Manager (usbip-device-manager)");
    info("---------------------------------------------------");
}

#[allow(dead_code)]
pub fn debug(msg: &str) {
    write_line("DEBUG", msg);
}

pub fn info(msg: &str) {
    write_line("INFO", msg);
}

pub fn warn(msg: &str) {
    write_line("WARN", msg);
}

pub fn error(msg: &str) {
    write_line("ERROR", msg);
}

/// Delete all log files. Fails silently on files currently in use.
pub fn clear_history() -> usize {
    let dir = logs_dir();
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

pub fn open_dir() {
    let dir = config::app_data_dir();
    if dir.exists() {
        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    }
}
