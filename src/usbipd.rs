// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! usbipd-win integration: installation check, device listing and
//! bind/unbind/attach/detach operations (ported from the C# implementation).

use crate::config::UsbDevice;
use crate::lang;
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
        if privileged {
            match sys::run_elevated(&self.exe, &arg_list, 10_000) {
                Ok(Some(code)) => Ok(CmdOut {
                    code: exit_code_to_err_code(code as i32),
                    stdout: String::new(),
                    stderr: String::new(),
                }),
                Ok(None) => Err(lang::t("CanceledByUser")),
                Err(e) => Err(e),
            }
        } else {
            let out = sys::run_process(&self.exe, &arg_list, 10_000)?;
            let code = out
                .exit_code
                .map(exit_code_to_err_code)
                .unwrap_or(ErrCode::Timeout);
            let (stdout, stderr) = strip_prefixes(&format!("{}\n{}", out.stdout, out.stderr));
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
}

// ---------------------------------------------------------------------------
// Daemon process manager (usbipd attach --auto-attach)
// ---------------------------------------------------------------------------

pub struct DaemonManager {
    children: Vec<DaemonProc>,
}

struct DaemonProc {
    pid: u32,
    args: String,
}

impl DaemonManager {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn prune(&mut self) {
        self.children.retain(|c| sys::is_process_running(c.pid));
    }

    /// true when an `attach` daemon whose arguments contain `needle` is alive.
    pub fn attaching(&mut self, needle: &str) -> bool {
        self.prune();
        self.children.iter().any(|c| {
            c.args.contains("attach") && c.args.to_lowercase().contains(&needle.to_lowercase())
        })
    }

    pub fn spawn_daemon(&mut self, exe: &Path, args: &[&str]) -> Result<(), String> {
        self.prune();
        let joined = args.join(" ");
        if self
            .children
            .iter()
            .any(|c| c.args.eq_ignore_ascii_case(&joined))
        {
            return Ok(()); // already running
        }
        let child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start auto-attach daemon: {e}"))?;
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
        }
        self.children
            .retain(|c| !c.args.to_lowercase().contains(&lower) || !sys::is_process_running(c.pid));
    }

    pub fn stop_all(&mut self) {
        let pids: Vec<u32> = self.children.iter().map(|c| c.pid).collect();
        for pid in pids {
            let _ = sys::kill_pid(pid);
        }
        self.children.clear();
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

pub fn bind_device(
    usbipd: &Usbipd,
    dev: &UsbDevice,
    use_bus_id: bool,
    force: bool,
) -> Result<(), String> {
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
    if dev.is_bound {
        return Ok(());
    }

    let mut last = usbipd.bind(&id, use_bus_id, force)?;
    let mut bound = false;
    for _ in 0..3 {
        if let Ok(list) = usbipd.list_devices() {
            if let Some(updated) = find_device(&list, &dev.hardware_id) {
                bound = updated.is_bound;
                if bound {
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        last = usbipd.bind(&id, use_bus_id, force)?;
    }
    if bound {
        Ok(())
    } else if last.code != ErrCode::Success {
        Err(format!("Failed to bind: {}", last.stderr))
    } else {
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
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_bound {
        return Ok(());
    }
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
        Ok(())
    } else if out.code != ErrCode::Success {
        Err(format!("Failed to unbind: {}", out.stderr))
    } else {
        Err("Failed to unbind the device.".to_owned())
    }
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
            return Err(format!("Device({id}) is not bound."));
        }
    }
    if cur.is_attached {
        return Ok(String::new());
    }

    let in_progress = daemons
        .lock()
        .map(|mut dm| dm.attaching(&id))
        .unwrap_or(false);
    if !in_progress {
        let out = usbipd.attach(&id, use_bus_id, host_ip)?;
        if out.code != ErrCode::Success {
            return Err(translate_attach_error(&out.stderr));
        }
    }

    let mut attached = false;
    for i in 1..=10 {
        if let Ok(list) = usbipd.list_devices() {
            if let Some(u) = find_device(&list, &cur.hardware_id) {
                attached = u.is_attached;
                if attached {
                    break;
                }
            }
        }
        if !in_progress && i % 3 == 0 {
            let out = usbipd.attach(&id, use_bus_id, host_ip)?;
            if out.code != ErrCode::Success {
                return Err(translate_attach_error(&out.stderr));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if attached {
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
                    return Err(e);
                }
            }
        }
        Ok(lang::t("AutoAttachDaemonExit"))
    } else {
        Err(translate_attach_error(&lang::t("ErrMsgAttachFail")))
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
        return Err(format!(
            "{} is empty.",
            if use_bus_id { "BusID" } else { "HardwareID" }
        ));
    }
    if !dev.is_connected {
        return Err(format!("Device({id}) is not connected."));
    }
    if !dev.is_attached {
        return Ok(());
    }
    let out = usbipd.detach(&id, use_bus_id)?;
    let mut detached = false;
    for _ in 0..10 {
        if let Ok(list) = usbipd.list_devices() {
            if let Some(u) = find_device(&list, &dev.hardware_id) {
                detached = !u.is_attached;
                if detached {
                    break;
                }
            } else {
                detached = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Ok(mut dm) = daemons.lock() {
        dm.stop_matching(&dev.hardware_id);
    }
    if detached {
        Ok(())
    } else if out.code != ErrCode::Success {
        Err(format!("Failed to detach: {}", out.stderr))
    } else {
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
