# usbip-device-manager Design Document

> Applies to: v0.1.0 (`usbip-device-manager.exe`)
> Display name: USBIP Device Manager (中文: USBIP 设备管理器)
> Last updated: 2026-09-03
> Language: [English](design.en.md) · [中文](design.md)

This document describes the overall design of `usbip-device-manager`:
architecture, threading model, UI layout, usbipd-win integration, and the key
design decisions behind it, to make future maintenance and review easier.

## 1. Background and Goals

This project is a **Rust + egui** rewrite of the .NET WPF
[wsl-usb-manager](https://github.com/zcj20080882/wsl-usb-manager). It uses
[usbipd-win](https://github.com/dorssel/usbipd-win) to bind USB devices on
Windows and attach them to WSL 2.

Design goals:

- Feature and interaction parity with the original C# app: three pages,
  both checkbox and context-menu operation entry points, device hiding,
  auto attach, tray support, and a compatible config/log directory.
- No dependency on .NET or PowerShell: the device list is read directly from
  the native `usbipd.exe state` JSON output, keeping refresh and hot-plug
  response latency low.
- The UI provides dark/light themes with consistent typography and colors,
  and an embedded CJK font so Chinese and English render identically.
- Reuse the original `%APPDATA%\WSL USB Manager` config directory so existing
  users do not lose their settings after upgrading.

## 2. Technology Stack and Dependencies

| Dependency | Purpose |
| --- | --- |
| `eframe` / `egui` 0.36 (glow) | Immediate-mode GUI, window and view layer |
| `tray-icon` 0.24 | System tray icon and menu |
| `image` | Loading/resizing the tray and window icons |
| `serde` / `serde_json` | `config.json` and `usbipd state` JSON parsing |
| `winreg` | Looking up usbipd-win installation in the registry |
| `windows-sys` 0.61 | ShellExecuteEx, registry, network cards, locale, WM_DEVICECHANGE broadcasts, etc. |
| `embed-resource` (build) | Embedding `appicon.ico` into the executable resources |

Windows 10+ only; Rust edition 2024. No .NET or PowerShell dependency.

## 3. Directory Structure and Module Responsibilities

```text
src/
  main.rs      Entry point: single instance, CJK fonts, window options, eframe startup
  app.rs       App state, background workers, message dispatch, actions, tray
  ui.rs        egui rendering: pages/lists/context menus/info area/settings/dialog/toasts
  usbipd.rs    usbipd-win detection, `usbipd state` JSON parsing, bind/attach, daemons
  config.rs    Config structures (PascalCase JSON, compatible with the original C# app)
  sys.rs       Win32 helpers: elevation, registry, network cards, single instance, locale
  monitor.rs   USB hot-plug broadcast watcher (trigger only, no device lookup)
  lang.rs      Chinese/English strings and current language state
  theme.rs     Dark/light theme
  log.rs       File logger
assets/
  appicon.ico  Executable icon (embedded by build.rs)
  appicon.png  Window and tray icon (loaded at runtime)
  fonts/       Noto Sans CJK SC (embedded via include_bytes)
```

| Module | Main responsibilities |
| --- | --- |
| `main.rs` | Log init, single-instance mutex, viewport setup, font setup, eframe launch |
| `app.rs` | `App` state; `UiMsg` handling; `Action` dispatch; background workers; tray creation |
| `ui.rs` | Per-frame rendering; page layout with independent scroll areas; context menus; dialogs and toasts |
| `usbipd.rs` | `Usbipd::check()`, `list_devices()` (JSON), bind/unbind/attach/detach, `DaemonManager` |
| `sys.rs` | `run_elevated`, `find_installed_app`, `network_cards`, single instance, locale |
| `monitor.rs` | Hidden window with device notification; emits a coalesced "state check needed" signal (no device lookup) |
| `config.rs` | `SystemConfig`/`AppConfig`/`UsbDevice` and read/write helpers, paths |
| `theme.rs` | Dark/light `Visuals`/`Style` and theme-aware color getters |
| `lang.rs` | String keys to Chinese/English; `lang::t()`; startup language detection |
| `log.rs` | Appends to `%APPDATA%\WSL USB Manager\Logs\yyyyMMdd.log` |

## 4. Startup Flow and Lifecycle

1. `log::init()`: create the log directory and write the startup marker.
2. `sys::acquire_single_instance()`: a named mutex
   (`usbip-device-manager-<guid>`). If it already exists, show a message and
   exit, preventing two instances from driving usbipd concurrently.
3. Load `config.json`; when no language is configured, pick the initial
   language from the system locale (`GetUserDefaultLocaleName` starting with
   `zh`).
4. Create the eframe window (title bar icon == tray icon) and start:
   - a `usbipd-check` thread running `Usbipd::check()`;
   - the `UsbMonitor` hidden-window USB broadcast listener (trigger only);
   - the tray icon and its menu event thread.
5. On `UiMsg::UsbipdReady`, the app becomes ready and triggers the first list
   refresh.
6. Exit: via Exit button or window close (close-to-tray hides instead);
   `App::drop` stops auto-attach daemons, the monitor and the tray.

## 5. UI Design

### 5.1 Overall Layout

- Top bar: page tabs on the left (Device / Persisted / Auto Attach); on the
  right, in order, Refresh, theme toggle (Light/Dark), the language menu
  (中文 / English), Settings and Exit. A spinner is shown and Refresh is
  disabled while busy.
- Switching tabs triggers a refresh and forces the default "visible devices
  only" state (`show_filtered = false`), so hidden devices stay hidden after
  the refresh.
- Language/theme/hide-filter changes are queued as `Action`s and executed
  after the current UI pass, avoiding state mutation during rendering.

### 5.2 Pages and Scrolling

Each page manages its own scroll areas (the window body itself does not
scroll):

| Page | List | Info area |
| --- | --- | --- |
| Device | Height fits content, capped at 2/3 of the window's usable height (reserving ~100 px when a device is selected) | Takes the remaining height; scrollbar appears automatically |
| Persisted | Same (2/3 cap + info-area reservation) | Same |
| Auto Attach | Fills the remaining height; scrollbar automatic | None |

- The list shows no scrollbar while below the cap (`max_height`, not a fixed
  height); it only appears when content exceeds the cap, so the list never
  stretches the window and the rest of the space goes to the info area.
- Each scroll area has a stable `id_salt` (e.g. `devices-list`,
  `devices-info`), preserving scroll offsets when switching tabs or after
  refreshes.
- Table rows are 30 px tall; headers use the same font size as rows and are
  bold; text cells are truncated.
- Context menu (Device page): Bind / Unbind, Attach / Detach,
  Hide / Show / Show hidden toggle, Add to Auto / Remove from Auto, with
  entries disabled according to device state; right-click also selects the row.
- The device info area shows InstanceId, HardwareId, BusId, PersistedGuid,
  IsBound/IsConnected/IsAttached, ClientIPAddress, etc. read-only and scrolls
  when content is long.

### 5.3 Theme

Defined in `theme.rs`:

- Complete dark/light `Visuals + Style` pairs with constants for background,
  panel, input, button, text, border colors; controls use a unified corner
  radius (6 px) and font sizes;
- Emphasized blue, error red and success green are consistent app-wide;
- Theme state lives in a `thread_local` (egui renders on a single thread);
  helpers such as `is_dark()` / `text()` are used throughout the UI;
- Toggling the theme updates the config, saves it and re-applies the theme.

### 5.4 Fonts and Icons

- `main.rs` embeds Noto Sans CJK SC (~16 MB) with `include_bytes`, inserted
  first in the Proportional family and as a Monospace fallback, so both
  languages render without depending on system fonts.
- `build.rs` embeds `appicon.ico` into the executable resources using
  `embed-resource` (Explorer/taskbar icon).
- At runtime, `assets/appicon.png` is used to generate a 64x64 window title
  icon and a 32x32 tray icon — the same image in both places.

### 5.5 Language

- The top bar language control is a popup menu (中文 / English) instead of two
  separate buttons;
- `lang.rs` maps keys to Chinese/English strings; `lang::t()` resolves against
  the current global language;
- The language is persisted; when unset it follows the system locale. On
  switch, the tray menu text is updated too (the app keeps `MenuItem` handles
  and calls `set_text`).

### 5.6 Tray

- Tray menu: Show / Exit. Closing the window hides it instead of exiting when
  "close to tray" is enabled.
- Tray events are listened to on a dedicated `tray-menu` thread and routed
  back to the UI thread as `UiMsg`, followed by `request_repaint()`.

## 6. Configuration and Data Model

`config.rs` keeps the original C# app's PascalCase JSON layout; the file lives
at `%APPDATA%\WSL USB Manager\config.json` (deliberately not tied to the new
name so old settings keep working).

```json
{
  "AppConfig": { "DarkMode": false, "Lang": "zh", "CloseToTray": true,
                 "UseBusID": false, "SpecifyNetCard": false,
                 "ForwardNetCard": "" },
  "AutoAttachDeviceList": [ { "HardwareId": "...", ... } ],
  "FilteredDeviceList": []
}
```

`UsbDevice` is the core device model:

| Field | Meaning | Main source |
| --- | --- | --- |
| `instance_id` | Windows device instance ID | usbipd state JSON |
| `hardware_id` | `VID:PID` (lowercase hex, e.g. `048d:5702`) | Derived from InstanceId |
| `description` | Device description | JSON |
| `is_forced` | Whether forced bind is set | JSON |
| `bus_id` | Bus ID (present while connected) | JSON |
| `persisted_guid` | Persisted GUID (present when bound) | JSON |
| `stub_instance_id` | usbip stub device instance | JSON |
| `client_ip_address` | Client IP (present when attached) | JSON |
| `is_bound` / `is_connected` / `is_attached` | Derived state | GUID / BusId / IP presence |

- `AutoAttachDeviceList` and `FilteredDeviceList` are keyed by
  `hardware_id` with case-insensitive comparison;
- Checking "Auto Attach" immediately binds and attaches the device (see 7.5)
  and writes it to the auto-attach list;
- "Hide" writes to the filtered list; context-menu "Show" or the show-hidden
  toggle removes/temporarily reveals entries.

## 7. usbipd-win Integration Design

### 7.1 Installation Detection and Version

- `sys::find_installed_app("usbipd-win")` scans the HKLM/HKCU Uninstall keys
  (including Wow6432Node) for the DisplayName to obtain InstallLocation and
  the version;
- `usbipd.exe` must exist and the version must be >= 4.4.0 (major.minor
  comparison); otherwise the user is asked to install/upgrade and restart;
- A warning is shown when the installation folder is not on a local drive
  (usbipd on a remote disk is not usable).

### 7.2 Device List: Native `usbipd state` JSON (Plan A)

Earlier versions queried the list through
`Import-Module …; Get-UsbipdDevice` in PowerShell, cold-starting PowerShell on
every refresh/hot-plug event, which added noticeable latency. The app now
directly runs:

```text
usbipd.exe state
```

The output is JSON (supported since usbipd-win 2.2.0; this project requires
>= 4.4.0). `Get-UsbipdDevice` is only a wrapper around that same JSON.
Implementation notes:

- `serde` deserializes against the official field names (`ClientIPAddress`
  needs an explicit `rename`);
- Devices with an empty InstanceId are filtered out;
- `IsBound`/`IsConnected`/`IsAttached` are not part of the JSON and are
  derived from whether `PersistedGuid`/`BusId`/`ClientIPAddress` are present;
- `hardware_id` is extracted from InstanceId (`VID_xxxx&PID_xxxx`,
  case-insensitive, exactly 4 hex digits, no hex digit directly after the
  PID) and normalized to lowercase `vid:pid` — matching what the PowerShell
  module displays and the format `usbipd bind --hardware-id` expects (same
  rules as the official `VidPid.TryParseId`);
- stdout is UTF-8, so Chinese descriptions parse cleanly; 8-second timeout;
  non-zero exit codes report stderr.

Measured on a development machine: `usbipd.exe state` averages ~61 ms versus
~276 ms for the old PowerShell path (roughly 4.5x faster per call). Refreshes,
hot-plug refreshes and post-operation verification polls all use this path.

### 7.3 bind / unbind: Elevation Design

Binding/unbinding writes registry/driver-related state and therefore needs
administrator rights. Current implementation:

- `usbipd.exe bind|unbind --hardware-id|--busid …` runs through
  `sys::run_elevated()`;
- `run_elevated` uses `ShellExecuteExW` with the `runas` verb to show the
  standard UAC prompt, waits for the process with a 10-second timeout; a user
  cancel (`ERROR_CANCELLED`) maps to "the administrator prompt was declined";
- `--force` is added to bind as needed; afterwards `state` is polled to
  confirm the device state before reporting success.

Limitation: in enterprise environments where privilege management (EPM)
software takes over UAC, success depends on policy; the app cannot bypass it.
Planned improvement (not implemented): detect whether the current process is
already elevated, and if so launch `usbipd.exe` directly with the current
token instead of triggering a second UAC/EPM prompt.

### 7.4 attach / detach

- attach: `usbipd attach (--busid|--hardware-id) --wsl [--host-ip …]`
  (no elevation needed); common errors (firewall, no running WSL, device in
  use by Windows) are translated into localized messages; success is
  confirmed by polling `IsAttached`.
- detach: `usbipd detach …`, confirmed by polling, then the matching
  auto-attach daemon is stopped.
- When a network card is selected, attach uses `--host-ip <IPv4>`. Cards are
  enumerated with `GetAdaptersAddresses` (loopback/tunnel excluded; only
  operational IPv4 interfaces are returned).

### 7.5 Auto Attach

- Ticking Auto Attach immediately triggers "bind (if not bound) + sleep 1 s +
  attach";
- on success a long-running `usbipd attach … --auto-attach` process is
  started (`DaemonManager` records PID and arguments; zombies are pruned with
  `is_process_running`, stop uses `taskkill /T /F`), so usbipd keeps watching
  the device across unplug/replug;
- every idle frame, the app auto-attaches devices that are connected, in the
  auto list, not attached, and not triggered in the last 8 seconds; both the
  row checkbox and the context menu can drive this.

### 7.6 Post-Operation Consistency Checks

After bind/unbind/attach/detach, `list_devices()` (`usbipd state`) is polled
to confirm the change instead of trusting only the exit code:

- bind: up to 3 rounds, 500 ms apart, until `IsBound`;
- attach: up to 10 rounds, 500 ms apart, until `IsAttached`;
- detach: likewise 10 rounds until `!IsAttached`.

## 8. USB Hot-Plug Monitoring

Principle: **Windows broadcasts are only a trigger; device add/remove is
decided by diffing consecutive `usbipd state` snapshots**. Windows PnP
notifications produce node add/remove and shadow-device swaps during attach
(e.g. `VID_0403` ↔ `VID_80EE`) plus dense 0x7 broadcasts, so they cannot be
reliably mapped back to a physical plug/unplug. `usbipd state` is the same
source the UI renders from, so it cannot misreport.

1. `monitor.rs` registers device notifications on a hidden top-level window;
   on `WM_DEVICECHANGE` (0x8000/0x8004/0x7) it only coalesces for 200 ms and
   sends `UiMsg::UsbActivity` -- no enumeration, no device identity;
2. In the UI frame `flush_usb_activity` waits out a 900 ms storm cooldown,
   then the `refresh` thread runs one `list_devices()` and the result is
   diffed against the previous snapshot;
3. An unchanged snapshot rebuilds/repaints nothing (kills the endless
   refresh after a successful attach);
4. A changed snapshot rebuilds all three pages and repaints; a device that
   vanished while attached stops its auto-attach daemon by busid/VID:PID;
   new devices are picked up by `maybe_auto_attach` (with a delayed
   re-evaluation once the 8 s debounce expires);
5. Because `usbipd state` lags the broadcast slightly, an unchanged snapshot
   after a broadcast triggers up to 2 extra checks 600 ms apart;
6. A physical unplug of a device attached to WSL may produce no Windows
   notification at all; `poll_state_sync` covers that with a 1 s fallback
   diff, active only while attached devices exist and repainting only on
   real changes.

The old "~7 s delay after plug-in" was caused by cold-starting PowerShell
(~276 ms each) for every event, so event bursts launched competing PowerShell
processes. Debouncing plus a single native `usbipd state` call dropped the
latency noticeably.

## 9. Threading Model and Messaging

egui is single-threaded for rendering; all slow work happens on background
threads and results come back over `std::sync::mpsc`, drained in batches every
frame by the UI thread.

| Thread | Responsibility | Trigger |
| --- | --- | --- |
| UI main thread (`logic`/`ui`) | Rendering, message draining, USB activity flush, fallback polling, Action dispatch | Always running |
| `usbipd-check` | Installation/version detection | Once at startup |
| `refresh` | `list_devices()` refresh | Manual refresh/tab switch/init |
| `usbipd-op` | One-shot bind/unbind/attach/detach operations | Per user operation |
| `usb-monitor` | WM_DEVICECHANGE broadcast listener (200 ms coalescing) | App lifetime |
| `tray-menu` | Tray menu events | While the tray exists |

Core messages (`UiMsg`):

| Message | Meaning and handling |
| --- | --- |
| `UsbipdReady` | usbipd check done: mark ready, trigger first refresh |
| `Lists(Vec<UsbDevice>)` | State snapshot: diff-driven decisions (stop daemon/auto attach/toast), rebuild only on change |
| `ListFailed` | State fetch failed: notify but keep the current list |
| `OpDone` | Operation result: toast/error dialog, then refresh |
| `Error` | Error: log + dialog |
| `UsbActivity` | USB broadcast signal: coalesce, then one state check |
| `ShowWindow` / `ExitApp` | Tray commands |

The `busy` flag serializes work: clicking Refresh during an operation only
sets `need_refresh`, which runs after the current task finishes, avoiding
concurrent usbipd calls; while busy the app repaints every 120 ms (spinner).

## 10. Error Handling and Logging

- User-operation errors are shown in a modal dialog (`Dialog`); short
  success/info messages use toasts;
- Background failures are unified through `UiMsg::Error`, logged and shown as
  a dialog on the UI thread;
- Logs go to `%APPDATA%\WSL USB Manager\Logs\yyyyMMdd.log` with timestamp,
  thread name and level; Settings can clear the logs or open the folder;
- Critical paths have timeouts (8–10 s) and exit-code classification
  (`ExitCodeToErrCode`: 0 success, 2/3 not found, 5/126 access denied, etc.).

## 11. Build, Test and Release

```powershell
cargo test          # unit tests: JSON parsing, version parsing, config PascalCase, theme
cargo build --release
```

Artifact: `target\release\usbip-device-manager.exe`.

Test coverage highlights:

- `usbipd state` JSON: null fields, empty-InstanceId filtering, VID/PID
  case and length boundaries, derived flags;
- the old PowerShell text parser is kept as a `#[cfg(test)]` reference test;
- version parsing (`4.4.0`, `v5.2.1-preview`);
- config.json round-trips against the original C# PascalCase shape;
- both themes have readable colors.

## 12. Known Limitations and Future Improvements

- bind/unbind depends on UAC/EPM approval (see 7.3); a "run usbipd directly
  when the process is already elevated" check is a future addition;
- USB plug/unplug relies on `usbipd state` diffs: when a device attached to
  WSL is physically removed with no Windows broadcast at all, discovery can
  take up to the 1 s fallback poll;
- single instance / single usbipd workflow is a deliberate trade-off to avoid
  concurrent commands interfering;
- the config directory intentionally stays `WSL USB Manager` for compatibility;
- a PowerShell fallback parser is deliberately not implemented; it could be
  added later if support for usbipd older than 4.4.0 (no `state` JSON) is
  needed.
