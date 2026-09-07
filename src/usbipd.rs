// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! usbipd-win integration: installation check, device listing and
//! bind/unbind/attach/detach operations (ported from the C# implementation).

use crate::config::UsbDevice;
use crate::lang;
use crate::log;
use crate::sys;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

pub const USBIPD_APP_NAME: &str = "usbipd-win";
pub const USBIPD_MIN_MAJOR: u32 = 4;
pub const USBIPD_MIN_MINOR: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ErrCode {
    DeviceDetachFailed = -26,
    DeviceUnbindFailed = -25,
    DeviceAttachFailed = -24,
    DeviceBindFailed = -23,
    DeviceNotAttached = -22,
    DeviceNotBound = -21,
    DeviceNotConnected = -20,
    UsbipdLowVersion = -11,
    UsbipdNotFound = -10,
    WslDistribNotFound = -4,
    WslLowVersion = -3,
    WslNotRunning = -2,
    WslNotInstalled = -1,
    Success = 0,
    Failure = 1,
    ParseError = 2,
    AccessDenied = 3,
    Timeout = 4,
    DeviceInAttaching = 5,
    UnknownError = 255,
}

fn exit_code_to_err_code(code: i32) -> ErrCode {
    match code {
        0 => ErrCode::Success,
        1 => ErrCode::Failure,
        2 | 3 => ErrCode::UsbipdNotFound,
        5 | 126 => ErrCode::AccessDenied,
        _ => ErrCode::UnknownError,
    }
}

/// Result of a usbipd command after stripping `info:`/`error:`/`warning:` prefixes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CmdOut {
    pub code: ErrCode,
    pub stdout: String,
    pub stderr: String,
}

impl Default for CmdOut {
    fn default() -> Self {
        Self {
            code: ErrCode::Success,
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

fn strip_prefixes(text: &str) -> (String, String) {
    let mut out = String::new();
    let mut err = String::new();
    for line in text.lines() {
        let l = line.trim_end_matches('\r');
        if let Some(idx) = l.find("info:") {
            out.push_str(l[idx + 5..].trim());
            out.push('\n');
        } else if let Some(idx) = l.find("error:") {
            err.push_str(l[idx + 6..].trim());
            err.push('\n');
        } else if let Some(idx) = l.find("warning:") {
            err.push_str(l[idx + 8..].trim());
            err.push('\n');
        } else if !l.trim().is_empty() {
            out.push_str(l);
            out.push('\n');
        }
    }
    (out.trim_end().to_owned(), err.trim_end().to_owned())
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Usbipd {
    pub exe: PathBuf,
    pub version: String,
    pub install_dir: PathBuf,
}

impl Usbipd {
    /// Locate usbipd-win and verify the minimum version (4.4.0, major.minor
    /// comparison like the original). `Ok((client, warning))`.
    pub fn check() -> Result<(Usbipd, Option<String>), String> {
        let info = sys::find_installed_app(USBIPD_APP_NAME)
            .ok_or_else(|| lang::t("UsbipdNotInstalled"))?;

        let install_dir = PathBuf::from(info.install_location.trim());
        if !install_dir.is_dir() {
            return Err(lang::t("UsbipdNotInstalled"));
        }
        let exe = install_dir.join("usbipd.exe");
        if !exe.is_file() {
            return Err(lang::t("UsbipdNotInstalled"));
        }

        let mut version = info.display_version.trim().to_owned();
        if version.is_empty() {
            let args = vec!["--version".to_owned()];
            if let Ok(out) = sys::run_process(&exe, &args, 10_000) {
                version = out.stdout.trim().to_owned();
            }
        }

        let (major, minor) = parse_version(&version)
            .ok_or_else(|| format!("Failed to parse usbipd version: {version}"))?;
        if major < USBIPD_MIN_MAJOR || (major == USBIPD_MIN_MAJOR && minor < USBIPD_MIN_MINOR) {
            return Err(lang::t("UsbipdLowVersion"));
        }

        let mut warning = None;
        let loc = install_dir.to_string_lossy().to_owned();
        let is_local = loc.len() >= 3
            && loc.as_bytes()[0].is_ascii_alphabetic()
            && loc.as_bytes()[1] == b':'
            && loc.as_bytes()[2] == b'\\';
        if !is_local {
            warning = Some(lang::t("UsbipdRemoteDisk"));
        }

        Ok((
            Usbipd {
                exe,
                version: format!("{major}.{minor}"),
                install_dir,
            },
            warning,
        ))
    }

    /// Run usbipd.exe. `privileged = true` triggers a UAC prompt.
    pub fn run(&self, args: &[&str], privileged: bool) -> Result<CmdOut, String> {
        let arg_list: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        log::info(&format!(
            "usbipd run: {} privileged={privileged}",
            args.join(" ")
        ));
        if privileged {
            match sys::run_elevated(&self.exe, &arg_list, 10_000) {
                Ok(Some(code)) => {
                    log::info(&format!("usbipd (elevated) exit code: {code}"));
                    Ok(CmdOut {
                        code: exit_code_to_err_code(code as i32),
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                }
                Ok(None) => {
                    log::info("usbipd (elevated) canceled by user");
                    Err(lang::t("CanceledByUser"))
                }
                Err(e) => {
                    log::error(&format!("usbipd (elevated) launch failed: {e}"));
                    Err(e)
                }
            }
        } else {
            let out = sys::run_process(&self.exe, &arg_list, 10_000)?;
            let code = out
                .exit_code
                .map(exit_code_to_err_code)
                .unwrap_or(ErrCode::Timeout);
            let (stdout, stderr) = strip_prefixes(&format!("{}\n{}", out.stdout, out.stderr));
            log::info(&format!(
                "usbipd exit: {} -> code={:?}, stderr={}",
                args.join(" "),
                code,
                stderr.trim()
            ));
            Ok(CmdOut {
                code,
                stdout,
                stderr,
            })
        }
    }

    /// Full device list from the native `usbipd state` JSON output.
    ///
    /// This avoids starting a PowerShell process for every refresh; the
    /// PowerShell module (`Get-UsbipdDevice`) is just a wrapper around the
    /// same JSON data.
    pub fn list_devices(&self) -> Result<Vec<UsbDevice>, String> {
        let out = sys::run_process(&self.exe, &["state".to_owned()], 8_000)?;
        if out.timed_out {
            return Err("Failed to fetch USB device list: usbipd state timed out.".to_owned());
        }
        if out.exit_code != Some(0) {
            let msg = if out.stderr.trim().is_empty() {
                out.stdout.trim()
            } else {
                out.stderr.trim()
            };
            return Err(format!("Failed to fetch USB device list: {msg}"));
        }
        if out.stdout.trim().is_empty() {
            return Ok(Vec::new());
        }
        parse_state_json(&out.stdout)
    }

    pub fn bind(&self, id: &str, use_bus_id: bool, force: bool) -> Result<CmdOut, String> {
        let mut args = vec![
            "bind",
            if use_bus_id {
                "--busid"
            } else {
                "--hardware-id"
            },
            id,
        ];
        if force {
            args.push("--force");
        }
        self.run(&args, true)
    }

    pub fn unbind(&self, id: &str, use_bus_id: bool) -> Result<CmdOut, String> {
        self.run(
            &[
                "unbind",
                if use_bus_id {
                    "--busid"
                } else {
                    "--hardware-id"
                },
                id,
            ],
            true,
        )
    }

    pub fn attach(
        &self,
        id: &str,
        use_bus_id: bool,
        host_ip: Option<&str>,
    ) -> Result<CmdOut, String> {
        let mut args = vec![
            "attach",
            if use_bus_id {
                "--busid"
            } else {
                "--hardware-id"
            },
            id,
            "--wsl",
        ];
        if let Some(ip) = host_ip {
            args.push("--host-ip");
            args.push(ip);
        }
        self.run(&args, false)
    }

    pub fn detach(&self, id: &str, use_bus_id: bool) -> Result<CmdOut, String> {
        self.run(
            &[
                "detach",
                if use_bus_id {
                    "--busid"
                } else {
                    "--hardware-id"
                },
                id,
            ],
            false,
        )
    }
}

fn parse_version(v: &str) -> Option<(u32, u32)> {
    let mut parts = v
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u32>().ok())
        .take(2);
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    Some((major, minor))
}

// ---------------------------------------------------------------------------
// Device list parsing
// ---------------------------------------------------------------------------

/// Document produced by `usbipd.exe state`.
#[derive(Debug, Deserialize)]
struct StateJson {
    #[serde(rename = "Devices")]
    devices: Vec<DeviceStateJson>,
}

/// One entry of the `usbipd state` JSON array. Nullable fields are `None`;
/// the derived flags (`IsBound`/`IsConnected`/`IsAttached`) are not part of
/// the JSON and are computed in [`state_device_to_usb_device`].
#[derive(Debug, Default, Deserialize)]
struct DeviceStateJson {
    #[serde(default, rename = "BusId")]
    bus_id: Option<String>,
    #[serde(default, rename = "ClientIPAddress")]
    client_ip_address: Option<String>,
    #[serde(default, rename = "Description")]
    description: String,
    #[serde(default, rename = "InstanceId")]
    instance_id: String,
    #[serde(default, rename = "IsForced")]
    is_forced: bool,
    #[serde(default, rename = "PersistedGuid")]
    persisted_guid: Option<String>,
    #[serde(default, rename = "StubInstanceId")]
    stub_instance_id: Option<String>,
}

fn parse_state_json(text: &str) -> Result<Vec<UsbDevice>, String> {
    let state: StateJson = serde_json::from_str(text)
        .map_err(|e| format!("Failed to parse usbipd state JSON: {e}"))?;
    Ok(state
        .devices
        .into_iter()
        .filter_map(state_device_to_usb_device)
        .collect())
}

fn state_device_to_usb_device(d: DeviceStateJson) -> Option<UsbDevice> {
    if d.instance_id.trim().is_empty() {
        return None;
    }
    let is_bound = d
        .persisted_guid
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let is_connected = d.bus_id.as_deref().is_some_and(|s| !s.trim().is_empty());
    let is_attached = d
        .client_ip_address
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let instance_id = d.instance_id;
    // `Get-UsbipdDevice` displays the exact same lowercase `VID:PID`.
    let hardware_id = vid_pid_of(&instance_id).unwrap_or_else(|| "0000:0000".to_owned());
    Some(UsbDevice {
        instance_id,
        hardware_id,
        description: d.description,
        is_forced: d.is_forced,
        bus_id: d.bus_id.unwrap_or_default(),
        persisted_guid: d.persisted_guid.unwrap_or_default(),
        stub_instance_id: d.stub_instance_id.unwrap_or_default(),
        client_ip_address: d.client_ip_address.unwrap_or_default(),
        is_bound,
        is_connected,
        is_attached,
        name: String::new(),
    })
}

/// Extract `VID:PID` (lowercase, e.g. `0403:6001`) from a Windows instance
/// id. Mirrors `VidPid.TryParseId` in usbipd-win: the first `VID_xxxx&PID_xxxx`
/// token, case-insensitive, exactly 4 hex digits each, with no extra hex
/// digit immediately following the PID.
fn vid_pid_of(instance_id: &str) -> Option<String> {
    let s = instance_id.to_ascii_lowercase();
    let vid_at = s.find("vid_")?;
    let vid_hex = s.get(vid_at + 4..vid_at + 8)?;
    if !vid_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let after_vid = &s[vid_at + 8..];
    let pid_mark = after_vid.find("&pid_")? + 5;
    let pid_hex = after_vid.get(pid_mark..pid_mark + 4)?;
    if !pid_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if let Some(next) = after_vid.get(pid_mark + 4..).and_then(|t| t.chars().next()) {
        if next.is_ascii_hexdigit() {
            return None;
        }
    }
    Some(format!("{vid_hex}:{pid_hex}"))
}

/// Parser for the previous PowerShell module output (`Get-UsbipdDevice` text
/// blocks), kept under `cfg(test)` as reference for the old format.
#[cfg(test)]
fn parse_device_list(text: &str) -> Vec<UsbDevice> {
    let mut devices: Vec<UsbDevice> = Vec::new();
    let mut cur: Option<UsbDevice> = None;

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() {
            if let Some(d) = cur.take() {
                if !d.hardware_id.trim().is_empty() {
                    devices.push(d);
                }
            }
            continue;
        }
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().to_owned();
            let is_new = key.eq_ignore_ascii_case("InstanceId");
            if is_new {
                if let Some(d) = cur.take() {
                    if !d.hardware_id.trim().is_empty() {
                        devices.push(d);
                    }
                }
                let mut d = UsbDevice::default();
                d.instance_id = value;
                cur = Some(d);
                continue;
            }
            if let Some(d) = cur.as_mut() {
                apply_property(d, key, value);
            }
        } else if let Some(d) = cur.as_mut() {
            // Continuation of a wrapped value (usually Description).
            if !d.description.is_empty() && !d.description.ends_with(' ') {
                d.description.push(' ');
            }
            d.description.push_str(line);
        }
    }
    if let Some(d) = cur.take() {
        if !d.hardware_id.trim().is_empty() {
            devices.push(d);
        }
    }
    devices
}

#[cfg(test)]
fn apply_property(d: &mut UsbDevice, key: &str, value: String) {
    match key.to_ascii_lowercase().as_str() {
        "hardwareid" => d.hardware_id = value,
        "description" => d.description = value,
        "isforced" => d.is_forced = parse_bool(&value),
        "busid" => d.bus_id = value,
        "persistedguid" => d.persisted_guid = value,
        "stubinstanceid" => d.stub_instance_id = value,
        "clientipaddress" => d.client_ip_address = value,
        "isbound" => d.is_bound = parse_bool(&value),
        "isconnected" => d.is_connected = parse_bool(&value),
        "isattached" => d.is_attached = parse_bool(&value),
        _ => {}
    }
}

#[cfg(test)]
fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_list_blocks() {
        let sample = "\r\n
InstanceId : USB\\VID_046D&PID_C534\\1234567890
HardwareId : USB\\VID_046D&PID_C534
Description : Logitech Unifying Receiver, Extra
IsForced : False
BusId : 1-1
PersistedGuid : {00000000-0000-0000-0000-000000000000}
StubInstanceId : USB\\VID_046D&PID_C534\\6&2ef4ef13&0&1
ClientIPAddress :
IsBound : True
IsConnected : True
IsAttached : True

InstanceId : USB\\VID_1234&PID_5678\\ABCDEF
HardwareId : USB\\VID_1234&PID_5678
Description : Test Device
IsForced : True
BusId :
PersistedGuid : {11111111-1111-1111-1111-111111111111}
StubInstanceId :
ClientIPAddress : 172.17.32.1
IsBound : True
IsConnected : False
IsAttached : False
";
        let list = parse_device_list(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].hardware_id, "USB\\VID_046D&PID_C534");
        assert!(list[0].is_bound && list[0].is_connected && list[0].is_attached);
        assert!(!list[1].is_connected);
        assert!(list[1].is_forced);
        assert_eq!(list[1].client_ip_address, "172.17.32.1");
        assert_eq!(list[0].short_name(), "Logitech Unifying Receiver");
    }

    #[test]
    fn parses_version() {
        assert_eq!(parse_version("4.4.0"), Some((4, 4)));
        assert_eq!(parse_version("v5.2.1-preview"), Some((5, 2)));
        assert_eq!(parse_version("4"), Some((4, 0)));
        assert_eq!(parse_version("abc"), None);
    }

    #[test]
    fn parses_state_json_devices_and_flags() {
        let sample = r#"{
            "Devices": [
                {
                    "BusId": "1-42",
                    "ClientIPAddress": "1.2.3.4",
                    "Description": "testDescription",
                    "InstanceId": "USB\\VID_1234&PID_CDEF\\6&1&0",
                    "IsForced": true,
                    "PersistedGuid": "ad9d8376-6284-495e-a80b-ff1826d7447d",
                    "StubInstanceId": "USB\\VID_1234&PID_CDEF\\stub"
                },
                {
                    "BusId": null,
                    "ClientIPAddress": null,
                    "Description": "",
                    "InstanceId": "",
                    "IsForced": false,
                    "PersistedGuid": null,
                    "StubInstanceId": null
                }
            ]
        }"#;
        let list = parse_state_json(sample).unwrap();
        assert_eq!(list.len(), 1);
        let dev = &list[0];
        assert_eq!(dev.hardware_id, "1234:cdef");
        assert_eq!(dev.instance_id, "USB\\VID_1234&PID_CDEF\\6&1&0");
        assert_eq!(dev.description, "testDescription");
        assert_eq!(dev.bus_id, "1-42");
        assert_eq!(dev.persisted_guid, "ad9d8376-6284-495e-a80b-ff1826d7447d");
        assert_eq!(dev.client_ip_address, "1.2.3.4");
        assert!(dev.is_forced);
        assert!(dev.is_bound && dev.is_connected && dev.is_attached);
    }

    #[test]
    fn parses_state_json_null_means_flags_off() {
        let sample = r#"{
            "Devices": [
                {
                    "BusId": null,
                    "ClientIPAddress": null,
                    "Description": "Some Device",
                    "InstanceId": "USB\\Vid_80EE&Pid_CAFE\\x",
                    "IsForced": false,
                    "PersistedGuid": null,
                    "StubInstanceId": null
                }
            ]
        }"#;
        let list = parse_state_json(sample).unwrap();
        assert_eq!(list.len(), 1);
        let dev = &list[0];
        // VID/PID matching is case-insensitive (VBoxUSB uses Vid_/Pid_).
        assert_eq!(dev.hardware_id, "80ee:cafe");
        assert!(!dev.is_bound && !dev.is_connected && !dev.is_attached);
        assert!(!dev.is_forced);
        assert!(dev.bus_id.is_empty());
        assert!(dev.client_ip_address.is_empty());
        assert!(dev.persisted_guid.is_empty());
    }

    #[test]
    fn rejects_pid_longer_than_four_hex_digits() {
        assert!(vid_pid_of("USB\\VID_1234&PID_56789\\x").is_none());
        assert_eq!(
            vid_pid_of("USB\\VID_1234&PID_5678\\x"),
            Some("1234:5678".to_owned())
        );
        assert_eq!(
            vid_pid_of("USB\\VID_046D&PID_C534&MI_00\\6&2ef4ef13&0&0000"),
            Some("046d:c534".to_owned())
        );
    }

    #[test]
    fn recognizes_already_attached_as_success_like_race() {
        assert!(is_already_attached_error(
            "Device with busid '6-2' is already attached to a client."
        ));
        assert!(is_already_attached_error(
            "Device with hardware-id '0403:6001' is already attached."
        ));
        assert!(!is_already_attached_error("Device not found"));
    }
}

// ---------------------------------------------------------------------------
// Daemon process manager (usbipd attach --auto-attach)
// ---------------------------------------------------------------------------

pub struct DaemonManager {
    children: Vec<DaemonProc>,
    /// Job Object（KILL_ON_JOB_CLOSE）：本进程无论正常退出还是被强杀，
    /// 都会连带终止自动附加守护进程，避免遗留占着设备的孤儿进程。
    #[cfg(windows)]
    job: Option<usize>,
}

struct DaemonProc {
    pid: u32,
    args: String,
}

impl DaemonManager {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            #[cfg(windows)]
            job: create_kill_on_close_job(),
        }
    }

    pub fn prune(&mut self) {
        let before = self.children.len();
        self.children.retain(|c| sys::is_process_running(c.pid));
        if self.children.len() < before {
            log::info(&format!(
                "daemon prune: removed {} exited auto-attach process(es)",
                before - self.children.len()
            ));
        }
    }

    /// true when an `attach` daemon whose arguments contain `needle` is alive.
    pub fn attaching(&mut self, needle: &str) -> bool {
        self.prune();
        for c in &self.children {
            if c.args.contains("attach") && c.args.to_lowercase().contains(&needle.to_lowercase()) {
                log::info(&format!("daemon attaching: pid={} args={}", c.pid, c.args));
                return true;
            }
        }
        false
    }

    pub fn spawn_daemon(&mut self, exe: &Path, args: &[&str]) -> Result<(), String> {
        self.prune();
        let joined = args.join(" ");
        if self
            .children
            .iter()
            .any(|c| c.args.eq_ignore_ascii_case(&joined))
        {
            log::info(&format!("daemon already running, skip spawn: {joined}"));
            return Ok(()); // already running
        }
        let mut command = Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：自动附加守护进程是常驻进程，禁止其控制台窗口
            // 停留在桌面。
            command.creation_flags(0x0800_0000);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Failed to start auto-attach daemon: {e}"))?;
        #[cfg(windows)]
        if let Some(job) = self.job {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            use windows_sys::Win32::System::Threading::OpenProcess;
            const PROCESS_SET_QUOTA: u32 = 0x0100;
            const PROCESS_TERMINATE: u32 = 0x0001;
            unsafe {
                let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, child.id());
                if proc.is_null() {
                    log::info(&format!(
                        "auto-attach daemon exited before job assignment (pid={})",
                        child.id()
                    ));
                } else {
                    let ok = windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                        job as HANDLE,
                        proc,
                    );
                    CloseHandle(proc);
                    if ok == 0 {
                        log::warn(&format!(
                            "failed to assign auto-attach daemon to job object (pid={})",
                            child.id()
                        ));
                    }
                }
            }
        }
        log::info(&format!(
            "auto-attach daemon started: pid={} args={}",
            child.id(),
            joined
        ));
        self.children.push(DaemonProc {
            pid: child.id(),
            args: joined,
        });
        Ok(())
    }

    pub fn stop_matching(&mut self, needle: &str) {
        let lower = needle.to_lowercase();
        let victims: Vec<u32> = self
            .children
            .iter()
            .filter(|c| c.args.to_lowercase().contains(&lower))
            .map(|c| c.pid)
            .collect();
        for pid in victims {
            let _ = sys::kill_pid(pid);
            log::info(&format!("stopping auto-attach daemon: pid={pid}"));
        }
        self.children
            .retain(|c| !c.args.to_lowercase().contains(&lower) || !sys::is_process_running(c.pid));
    }

    pub fn stop_all(&mut self) {
        let pids: Vec<u32> = self.children.iter().map(|c| c.pid).collect();
        if !pids.is_empty() {
            log::info(&format!("stopping all auto-attach daemons: {pids:?}"));
        }
        for pid in pids {
            let _ = sys::kill_pid(pid);
        }
        self.children.clear();
    }
}

#[cfg(windows)]
fn create_kill_on_close_job() -> Option<usize> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            CloseHandle(job);
            return None;
        }
        Some(job as usize)
    }
}

impl Drop for DaemonManager {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(job) = self.job {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    job as windows_sys::Win32::Foundation::HANDLE,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// High level operations (with the same retry behavior as the C# code)
// ---------------------------------------------------------------------------

fn id_of(dev: &UsbDevice, use_bus_id: bool) -> String {
    if use_bus_id {
        dev.bus_id.clone()
    } else {
        dev.hardware_id.clone()
    }
}

fn find_device<'a>(list: &'a [UsbDevice], hardware_id: &str) -> Option<&'a UsbDevice> {
    list.iter()
        .find(|d| d.hardware_id.eq_ignore_ascii_case(hardware_id))
}

/// 是否存在“已附加”的设备：优先按 InstanceId 精确匹配；否则只要同
/// VID:PID（可能有多台同型号设备）中任意一台已附加即视为已附加。
fn is_attached_in(list: &[UsbDevice], hardware_id: &str, instance_id: &str) -> bool {
    list.iter().any(|d| {
        if !d.is_attached {
            return false;
        }
        if !instance_id.is_empty() && d.instance_id.eq_ignore_ascii_case(instance_id) {
            return true;
        }
        !hardware_id.is_empty() && d.hardware_id.eq_ignore_ascii_case(hardware_id)
    })
}

pub fn bind_device(
    usbipd: &Usbipd,
    dev: &UsbDevice,
    use_bus_id: bool,
    force: bool,
) -> Result<(), String> {
    let id = id_of(dev, use_bus_id);
    if id.trim().is_empty() {
        log::error("bind aborted: id is empty");
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_connected {
        log::warn(&format!("bind aborted: device {id} is not connected"));
        return Err(format!("Device({id}) is not connected."));
    }
    if dev.is_bound {
        log::info(&format!("bind skipped: device {id} is already bound"));
        return Ok(());
    }

    log::info(&format!(
        "bind start: id={id} force={force} hardware_id={}",
        dev.hardware_id
    ));
    let mut last = usbipd.bind(&id, use_bus_id, force)?;
    let mut bound = false;
    for round in 1..=3 {
        match usbipd.list_devices() {
            Ok(list) => {
                if let Some(updated) = find_device(&list, &dev.hardware_id) {
                    bound = updated.is_bound;
                    log::info(&format!("bind poll {round}: id={id} bound={bound}"));
                    if bound {
                        break;
                    }
                } else {
                    log::warn(&format!(
                        "bind poll {round}: device {} not found in usbipd state",
                        dev.hardware_id
                    ));
                }
            }
            Err(e) => log::warn(&format!("bind poll {round}: list_devices failed: {e}")),
        }
        std::thread::sleep(Duration::from_millis(500));
        last = usbipd.bind(&id, use_bus_id, force)?;
    }
    if bound {
        log::info(&format!("bind success: id={id}"));
        Ok(())
    } else if last.code != ErrCode::Success {
        let msg = format!("Failed to bind: {}", last.stderr);
        log::error(&msg);
        Err(msg)
    } else {
        log::error(&format!("bind failed after retries: id={id}"));
        Err("Failed to bind the device.".to_owned())
    }
}

pub fn unbind_device(
    usbipd: &Usbipd,
    dev: &UsbDevice,
    use_bus_id: bool,
    daemons: &Mutex<DaemonManager>,
) -> Result<(), String> {
    let id = id_of(dev, use_bus_id);
    if id.trim().is_empty() {
        log::error("unbind aborted: id is empty");
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_bound {
        log::info(&format!("unbind skipped: device {id} is not bound"));
        return Ok(());
    }
    log::info(&format!(
        "unbind start: id={id} connected={} hardware_id={}",
        dev.is_connected, dev.hardware_id
    ));
    let out = usbipd.unbind(&id, use_bus_id)?;
    let mut unbound = false;
    if !dev.is_connected {
        unbound = true;
    } else {
        if out.code != ErrCode::Success {
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Ok(list) = usbipd.list_devices() {
            if let Some(u) = find_device(&list, &dev.hardware_id) {
                unbound = !u.is_bound;
            } else {
                unbound = true;
            }
        }
    }
    if let Ok(mut dm) = daemons.lock() {
        dm.stop_matching(&dev.hardware_id);
    }
    if unbound {
        log::info(&format!("unbind success: id={id}"));
        Ok(())
    } else if out.code != ErrCode::Success {
        let msg = format!("Failed to unbind: {}", out.stderr);
        log::error(&msg);
        Err(msg)
    } else {
        log::error(&format!("unbind failed: id={id}"));
        Err("Failed to unbind the device.".to_owned())
    }
}

/// 重插后设备尚未就绪/被占用等可自动恢复的错误：等待后重试即可，
/// 不应立即判定失败。
fn is_transient_attach_error(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("device busy")
        || s.contains("used by windows")
        || s.contains("device not found")
        || s.contains("not found")
}

/// usbipd 提示“设备已附加到某客户端”（例如上一轮 attach 刚成功，或
/// --auto-attach 守护进程抢先完成）。这是“其实已经附加成功”的信号，
/// 不是失败：应通过 usbipd state 确认后按成功处理。
pub(crate) fn is_already_attached_error(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("already attached")
}

fn translate_attach_error(raw: &str) -> String {
    if lang::is_zh() {
        if raw.contains("A firewall appears to be blocking the connection") {
            return lang::t("FirewallBlocked");
        }
        if raw.contains("There is no WSL 2 distribution running") {
            return lang::t("NoWslRunning");
        }
        if raw.contains("The device appears to be used by Windows") {
            return lang::t("DeviceUsedByWindows");
        }
    }
    raw.to_owned()
}

/// 执行一次 `usbipd attach`，并处理 “already attached to a client” 竞态：
/// 返回该错误时等 50ms 复查 `usbipd state`，若设备确实已附加则返回成功
/// （不再把这条错误向上抛、触发“无法附加设备”弹窗）。
fn run_attach_cmd(
    usbipd: &Usbipd,
    id: &str,
    use_bus_id: bool,
    host_ip: Option<&str>,
    hardware_id: &str,
    instance_id: &str,
) -> Result<CmdOut, String> {
    let out = usbipd.attach(id, use_bus_id, host_ip)?;
    if out.code == ErrCode::Success || !is_already_attached_error(&out.stderr) {
        return Ok(out);
    }
    // usbipd 报告“已附加到客户端”：等 50ms 让状态收敛，再确认一次。
    std::thread::sleep(Duration::from_millis(50));
    if !hardware_id.is_empty() {
        if let Ok(list) = usbipd.list_devices() {
            if is_attached_in(&list, hardware_id, instance_id) {
                log::info(&format!(
                    "attach confirmed already attached (50 ms re-check): id={id}"
                ));
                return Ok(CmdOut {
                    code: ErrCode::Success,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
        }
    }
    log::info(&format!(
        "attach said already attached but state disagrees; will retry: id={id}"
    ));
    Ok(out)
}

pub fn attach_device(
    usbipd: &Usbipd,
    dev: &UsbDevice,
    use_bus_id: bool,
    auto: bool,
    host_ip: Option<&str>,
    daemons: &Mutex<DaemonManager>,
) -> Result<String, String> {
    let id = id_of(dev, use_bus_id);
    if id.trim().is_empty() {
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_connected {
        return Err(format!("Device({id}) is not connected."));
    }
    log::info(&format!(
        "attach start: id={id} auto={auto} use_bus_id={use_bus_id} host_ip={host_ip:?} \
         connected={} bound={} attached={}",
        dev.is_connected, dev.is_bound, dev.is_attached
    ));

    let mut cur = dev.clone();
    if !cur.is_bound {
        if !auto {
            return Err(format!("Device({id}) is not bound."));
        }
        // Auto attach binds the device first (mirrors the original app).
        bind_device(usbipd, &cur, use_bus_id, cur.is_forced)?;
        std::thread::sleep(Duration::from_millis(1000));
        if let Ok(list) = usbipd.list_devices() {
            if let Some(u) = find_device(&list, &cur.hardware_id) {
                cur = u.clone();
            }
        }
        if !cur.is_bound {
            log::error(&format!("attach aborted: id={id} is not bound"));
            return Err(format!("Device({id}) is not bound."));
        }
    }
    if cur.is_attached {
        log::info(&format!("attach skipped: id={id} is already attached"));
        return Ok(String::new());
    }

    // 竞态防护：调用方快照可能已过期（例如守护进程刚抢先附加成功），
    // 执行前用最新 usbipd state 再确认一次，已附加就直接跳过，
    // 把与其它守护进程/附加之间的竞态窗口缩到最小。
    if let Ok(list) = usbipd.list_devices() {
        if let Some(u) = find_device(&list, &cur.hardware_id) {
            cur = u.clone();
        }
    }
    if cur.is_attached {
        log::info(&format!(
            "attach skipped: id={id} is already attached (fresh state)"
        ));
        return Ok(String::new());
    }

    let in_progress = daemons
        .lock()
        .map(|mut dm| dm.attaching(&id))
        .unwrap_or(false);
    log::info(&format!(
        "attach daemon in progress for id={id}: {in_progress}"
    ));
    let mut attached = false;
    let mut last_err: Option<String> = None;
    let mut already_attached_seen = false;

    if !in_progress {
        let mut out = run_attach_cmd(
            usbipd,
            &id,
            use_bus_id,
            host_ip,
            &cur.hardware_id,
            &cur.instance_id,
        )?;
        // usbipd attach 返回成功即视为已附加：不再等 state 轮询确认，
        // 避免“命令已成功但 state 延迟/匹配歧义”导致 UI 延迟数秒。
        if out.code == ErrCode::Success {
            attached = true;
        }
        let mut attempt = 0;
        while out.code != ErrCode::Success && attempt < 5 {
            if is_already_attached_error(&out.stderr) {
                already_attached_seen = true;
                // 竞态且 50ms 复查未确认：交由下方 state 轮询继续确认。
                log::info(&format!(
                    "attach still reports already attached, verifying via state: id={id}"
                ));
                break;
            }
            if !is_transient_attach_error(&out.stderr) {
                return Err(translate_attach_error(&out.stderr));
            }
            attempt += 1;
            last_err = Some(out.stderr.clone());
            log::info(&format!(
                "attach transient failure (attempt {attempt}): id={id} stderr={}",
                out.stderr.trim()
            ));
            std::thread::sleep(Duration::from_millis(1000));
            // 重试前先看设备是否其实已经被附加（例如旧守护进程已完成）。
            if let Ok(list) = usbipd.list_devices() {
                if is_attached_in(&list, &cur.hardware_id, &cur.instance_id) {
                    attached = true;
                    break;
                }
            }
            out = run_attach_cmd(
                usbipd,
                &id,
                use_bus_id,
                host_ip,
                &cur.hardware_id,
                &cur.instance_id,
            )?;
            if out.code == ErrCode::Success {
                attached = true;
            }
        }
        if out.code == ErrCode::Success {
            attached = true;
        }
        if !attached && out.code != ErrCode::Success {
            if is_transient_attach_error(&out.stderr) {
                last_err = Some(out.stderr.clone());
            } else if !is_already_attached_error(&out.stderr) {
                return Err(translate_attach_error(&out.stderr));
            }
        }
    }

    for i in 1..=10 {
        if attached {
            break;
        }
        match usbipd.list_devices() {
            Ok(list) => {
                attached = is_attached_in(&list, &cur.hardware_id, &cur.instance_id);
                log::info(&format!("attach poll {i}: id={id} attached={attached}"));
                if attached {
                    break;
                }
                if !list.iter().any(|d| {
                    d.instance_id.eq_ignore_ascii_case(&cur.instance_id)
                        || d.hardware_id.eq_ignore_ascii_case(&cur.hardware_id)
                }) {
                    log::warn(&format!(
                        "attach poll {i}: device {} not found in usbipd state",
                        cur.hardware_id
                    ));
                }
            }
            Err(e) => log::warn(&format!("attach poll {i}: list_devices failed: {e}")),
        }
        // “already attached”说明 usbipd 侧已附加成功，只是 state 尚未反映
        // （常见于同 VID:PID 多设备匹配歧义）：只轮询等待，不再反复 attach。
        let stop_retrying = last_err.as_deref().is_some_and(is_already_attached_error);
        if !in_progress && !stop_retrying && (last_err.is_some() || i % 3 == 0) {
            log::info(&format!(
                "attach retry {i}: launching usbipd attach for id={id}"
            ));
            let out = run_attach_cmd(
                usbipd,
                &id,
                use_bus_id,
                host_ip,
                &cur.hardware_id,
                &cur.instance_id,
            )?;
            if out.code != ErrCode::Success {
                log::info(&format!(
                    "attach retry {i} still failing: id={id} stderr={}",
                    out.stderr.trim()
                ));
                if is_already_attached_error(&out.stderr) {
                    already_attached_seen = true;
                }
                last_err = Some(out.stderr.clone());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !attached && already_attached_seen {
        // usbipd 明确报告已附加，只是 state 轮询一直没确认（例如同
        // VID:PID 多设备）：按成功处理，避免拖住 UI 数秒。
        log::info(&format!(
            "attach: usbipd reports already attached; treating as success: id={id}"
        ));
        attached = true;
    }

    if attached {
        log::info(&format!("attach success: id={id} auto={auto}"));
        if auto {
            // Keep usbipd watching for replugs.
            let mut args = vec![
                "attach",
                if use_bus_id {
                    "--busid"
                } else {
                    "--hardware-id"
                },
                &id,
                "--wsl",
            ];
            if let Some(ip) = host_ip {
                args.push("--host-ip");
                args.push(ip);
            }
            args.push("--auto-attach");
            let exe = usbipd.exe.clone();
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let borrowed: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            if let Ok(mut dm) = daemons.lock() {
                if let Err(e) = dm.spawn_daemon(&exe, &borrowed) {
                    log::error(&format!("failed to start auto-attach daemon: {e}"));
                    return Err(e);
                }
            }
        }
        Ok(lang::t("AutoAttachDaemonExit"))
    } else {
        let detail = last_err.unwrap_or_default();
        let e = if detail.is_empty() {
            translate_attach_error(&lang::t("ErrMsgAttachFail"))
        } else {
            translate_attach_error(&detail)
        };
        log::error(&format!(
            "attach failed after polls: id={id} auto={auto} in_progress={in_progress} error={e}"
        ));
        Err(e)
    }
}

pub fn detach_device(
    usbipd: &Usbipd,
    dev: &UsbDevice,
    use_bus_id: bool,
    daemons: &Mutex<DaemonManager>,
) -> Result<(), String> {
    let id = id_of(dev, use_bus_id);
    if id.trim().is_empty() {
        log::error("detach aborted: id is empty");
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_connected {
        log::warn(&format!("detach aborted: device {id} is not connected"));
        return Err(format!("Device({id}) is not connected."));
    }
    if !dev.is_attached {
        log::info(&format!("detach skipped: id={id} is not attached"));
        return Ok(());
    }
    log::info(&format!(
        "detach start: id={id} hardware_id={}",
        dev.hardware_id
    ));
    let out = usbipd.detach(&id, use_bus_id)?;
    let mut detached = false;
    for i in 1..=10 {
        match usbipd.list_devices() {
            Ok(list) => {
                if let Some(u) = find_device(&list, &dev.hardware_id) {
                    detached = !u.is_attached;
                    log::info(&format!("detach poll {i}: id={id} detached={detached}"));
                    if detached {
                        break;
                    }
                } else {
                    detached = true;
                    log::info(&format!(
                        "detach poll {i}: device {} no longer in usbipd state",
                        dev.hardware_id
                    ));
                    break;
                }
            }
            Err(e) => log::warn(&format!("detach poll {i}: list_devices failed: {e}")),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Ok(mut dm) = daemons.lock() {
        dm.stop_matching(&dev.hardware_id);
    }
    if detached {
        log::info(&format!("detach success: id={id}"));
        Ok(())
    } else if out.code != ErrCode::Success {
        let msg = format!("Failed to detach: {}", out.stderr);
        log::error(&msg);
        Err(msg)
    } else {
        log::error(&format!("detach failed after polls: id={id}"));
        Err("Failed to detach the device.".to_owned())
    }
}

#[allow(dead_code)]
pub fn detach_all(usbipd: &Usbipd) -> Result<(), String> {
    let out = usbipd.run(&["detach", "--all"], false)?;
    if out.code == ErrCode::Success {
        Ok(())
    } else {
        Err(out.stderr)
    }
}
