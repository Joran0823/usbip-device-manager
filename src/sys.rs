// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Windows-specific helpers: elevated process, registry lookup, adapters,
//! USB device enumeration, locale and process helpers.

use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, GetLastError, HANDLE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

pub fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct CmdOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Run a process capturing stdout/stderr, with a timeout in milliseconds.
/// Returns `exit_code = None` when the process was killed on timeout.
pub fn run_process(program: &Path, args: &[String], timeout_ms: u64) -> Result<CmdOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：release 为无控制台 GUI，禁止控制台子进程
        // （usbipd.exe 等）新建可见控制台窗口。
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to start {}: {e}", program.display()))?;

    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = out_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = err_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                let out_buf = out_thread.join().unwrap_or_default();
                let err_buf = err_thread.join().unwrap_or_default();
                return Ok(CmdOutput {
                    exit_code: code,
                    stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                    stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                    timed_out: false,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = kill_pid(child.id());
                    let _ = child.wait();
                    let out_buf = out_thread.join().unwrap_or_default();
                    let err_buf = err_thread.join().unwrap_or_default();
                    return Ok(CmdOutput {
                        exit_code: None,
                        stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                        stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                        timed_out: true,
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("Failed to wait for {}: {e}", program.display()));
            }
        }
    }
}

pub fn kill_pid(pid: u32) -> std::io::Result<std::process::Output> {
    let mut command = Command::new("taskkill.exe");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：避免停止守护进程时闪现黑窗。
        command.creation_flags(0x0800_0000);
    }
    command.output()
}

/// Best-effort check whether a process with `pid` is still alive.
pub fn is_process_running(pid: u32) -> bool {
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess};

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        arg.to_owned()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

/// Run an executable elevated through ShellExecuteEx with the `runas` verb
/// (shows the UAC prompt). Returns the process exit code.
pub fn run_elevated(
    program: &Path,
    args: &[String],
    timeout_ms: u64,
) -> Result<Option<u32>, String> {
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    let exe = to_wide(&program.to_string_lossy());
    let verb = to_wide("runas");
    let mut params = String::new();
    for a in args {
        if !params.is_empty() {
            params.push(' ');
        }
        params.push_str(&quote_arg(a));
    }
    let params = to_wide(&params);

    unsafe {
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = exe.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.nShow = 0; // SW_HIDE

        if ShellExecuteExW(&mut sei) == 0 {
            let err = GetLastError();
            if err == ERROR_CANCELLED {
                return Ok(None);
            }
            return Err(format!(
                "Elevated launch failed (error 0x{err:08X}); maybe the app is not installed."
            ));
        }

        let handle = sei.hProcess;
        if handle.is_null() {
            return Err("Elevated launch failed: no process handle returned.".to_owned());
        }

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let r = WaitForSingleObject(handle, 100);
            if r == WAIT_OBJECT_0 {
                let mut code: u32 = 0;
                GetExitCodeProcess(handle, &mut code);
                CloseHandle(handle);
                return Ok(Some(code));
            }
            if r != WAIT_TIMEOUT {
                CloseHandle(handle);
                return Err(format!("Waiting for elevated process failed: {r}"));
            }
            if Instant::now() >= deadline {
                let _ = TerminateProcess(handle, 1);
                CloseHandle(handle);
                return Err("Elevated process timed out.".to_owned());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Single instance
// ---------------------------------------------------------------------------

pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Returns `Some(guard)` when this is the first instance, `None` otherwise.
pub fn acquire_single_instance(name: &str) -> Option<InstanceGuard> {
    let wide = to_wide(name);
    unsafe {
        let handle = CreateMutexW(ptr::null(), 0, wide.as_ptr());
        if handle.is_null() {
            return Some(InstanceGuard { handle });
        }
        let err = GetLastError();
        if err == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            None
        } else {
            Some(InstanceGuard { handle })
        }
    }
}

// ---------------------------------------------------------------------------
// Locale / message box
// ---------------------------------------------------------------------------

pub fn system_locale_is_chinese() -> bool {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 32];
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return false;
    }
    let s = String::from_utf16_lossy(&buf[..(len - 1) as usize]);
    s.to_ascii_lowercase().starts_with("zh")
}

pub fn message_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_OK, MessageBoxW};
    let t = to_wide(title);
    let m = to_wide(text);
    unsafe {
        MessageBoxW(ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_OK);
    }
}

// ---------------------------------------------------------------------------
// Installed application lookup (Uninstall registry keys)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct AppInfo {
    pub display_name: String,
    pub install_location: String,
    pub display_version: String,
    pub publisher: String,
}

fn search_registry(base_hive: &winreg::RegKey, path: &str, needle: &str) -> Option<AppInfo> {
    use winreg::enums::{KEY_READ, KEY_WOW64_64KEY};

    let Ok(key) = base_hive.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY) else {
        return None;
    };
    for name in key.enum_keys().flatten() {
        let Ok(sub) = key.open_subkey_with_flags(&name, KEY_READ | KEY_WOW64_64KEY) else {
            continue;
        };
        let display_name: String = sub.get_value("DisplayName").unwrap_or_default();
        if !display_name.is_empty() && display_name.to_lowercase().contains(&needle.to_lowercase())
        {
            return Some(AppInfo {
                display_name,
                install_location: sub.get_value("InstallLocation").unwrap_or_default(),
                display_version: sub.get_value("DisplayVersion").unwrap_or_default(),
                publisher: sub.get_value("Publisher").unwrap_or_default(),
            });
        }
    }
    None
}

pub fn find_installed_app(needle: &str) -> Option<AppInfo> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    const BASE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    search_registry(&hklm, BASE, needle)
        .or_else(|| {
            search_registry(
                &hklm,
                r"SOFTWARE\Wow6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
                needle,
            )
        })
        .or_else(|| search_registry(&hkcu, BASE, needle))
}

// ---------------------------------------------------------------------------
// Network adapters
// ---------------------------------------------------------------------------

/// Returns `(friendly name, IPv4 address)` for usable adapters.
pub fn network_cards() -> Vec<(String, String)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR_IN};

    let mut result = Vec::new();
    unsafe {
        let mut size: u32 = 0;
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let r = GetAdaptersAddresses(
            AF_UNSPEC as u32,
            flags,
            ptr::null(),
            ptr::null_mut(),
            &mut size,
        );
        if r != 0 && size == 0 {
            return result;
        }
        let mut buf: Vec<u8> = vec![0; size as usize];
        let r = GetAdaptersAddresses(
            AF_UNSPEC as u32,
            flags,
            ptr::null(),
            buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH,
            &mut size,
        );
        if r != 0 {
            return result;
        }

        let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            let adapter = &*cur;
            let oper_up = adapter.OperStatus == IfOperStatusUp;
            let if_type = adapter.IfType;
            let is_loopback_or_tunnel =
                if_type == IF_TYPE_SOFTWARE_LOOPBACK || if_type == IF_TYPE_TUNNEL;
            if oper_up && !is_loopback_or_tunnel {
                let mut ip: Option<String> = None;
                let mut ua = adapter.FirstUnicastAddress;
                while !ua.is_null() {
                    let addr = &*ua;
                    if !addr.Address.lpSockaddr.is_null() {
                        let sa = &*(addr.Address.lpSockaddr as *const SOCKADDR_IN);
                        if sa.sin_family == AF_INET {
                            // AF_INET
                            let addr4 = sa.sin_addr.S_un.S_addr;
                            let [b, c, d, e] = addr4.to_be_bytes();
                            ip = Some(format!("{b}.{c}.{d}.{e}"));
                            break;
                        }
                    }
                    ua = addr.Next;
                }
                if let Some(ip) = ip {
                    let name = from_wide(adapter.FriendlyName);
                    if !name.is_empty() {
                        result.push((name, ip));
                    }
                }
            }
            cur = adapter.Next;
        }
    }
    result.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    result
}

// ---------------------------------------------------------------------------
// USB device enumeration (SetupAPI)
// ---------------------------------------------------------------------------

/// Enumerate present USB PnP device nodes and return their full Windows
/// device instance ids (e.g. `USB\VID_0403&PID_6001\A50285BI`). The instance
/// id uniquely identifies a device instance and matches the `InstanceId`
/// reported by `usbipd state`.
pub fn usb_instance_ids() -> Vec<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_ALLCLASSES, DIGCF_PRESENT, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    };

    let mut ids = Vec::new();
    unsafe {
        let usb = to_wide("USB");
        let set = SetupDiGetClassDevsW(
            ptr::null(),
            usb.as_ptr(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if set == 0 {
            return ids;
        }
        let mut idx: u32 = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, idx, &mut data) == 0 {
                break;
            }
            idx += 1;

            let mut required: u32 = 0;
            SetupDiGetDeviceInstanceIdW(set, &data, ptr::null_mut(), 0, &mut required);
            if required == 0 {
                continue;
            }
            let mut buf = vec![0u16; required as usize + 2];
            if SetupDiGetDeviceInstanceIdW(
                set,
                &data,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut required,
            ) != 0
            {
                let id = from_wide(buf.as_ptr());
                if !id.trim().is_empty() {
                    ids.push(id);
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    ids.sort();
    ids.dedup();
    ids
}
