# usbip-device-manager

[简体中文](README.md) · [English](README.en.md)

A **Rust + egui** rewrite of [WSL USB Manager](https://github.com/zcj20080882/wsl-usb-manager)
(originally .NET WPF) that binds USB devices on Windows and attaches them to
WSL 2 through [usbipd-win](https://github.com/dorssel/usbipd-win).

📖 Design docs: [English](docs/design.en.md) · [中文](docs/design.md)

## Features

- Three pages: connected devices (Device), persisted devices (Persisted), and
  the auto-attach list (Auto Attach)
- Table columns: Auto Attach, Bus ID, Hardware ID, Bound, Attached, Force
  Bind, Description
- Both checkboxes and right-click context menus can bind/unbind/attach/detach
- Hide/show devices (filtered list), add to/remove from the auto-attach list
- A full device-info panel is shown for the selected device
- USB plug-and-play monitoring: auto refresh on plug/unplug; auto-listed
  devices are attached automatically when connected
- Device list is read from the native `usbipd.exe state` JSON output — no
  PowerShell dependency (bind/attach operations always called usbipd.exe
  directly)
- Settings: specify the network card (`--host-ip`), use BusID, close to tray
- 中文/English switching, light/dark themes
- Config and logs stay compatible with the original app:
  `%APPDATA%\WSL USB Manager\config.json`, `Logs\yyyyMMdd.log`
- Bind/unbind run elevated automatically (UAC prompt)

## Roadmap

The following items are planned for future releases and are not implemented
in the current version:

1. **USB/IP client mode (runnable inside a VM with a desktop environment)**:
   support discovering devices shared by a USB/IP server (for example, a
   Windows host running usbipd-win) and attaching them, so the app can run in
   a VM that has a desktop environment and pull host-shared USB devices into
   that VM.
2. **Attach devices to VMs hosted on Windows over SSH**: support accessing
   VMs on Windows through SSH and attaching USB devices to those VMs.

## Theme and Fonts

- `src/theme.rs` provides complete dark/light `Visuals + Style` pairs with
  rounded controls, consistent typography and colors
- Embedded Noto Sans CJK SC font (~15 MB); Chinese and English render
  identically without relying on system fonts
- Global body/button text 15 px, secondary text 13 px, titles 19 px; table
  rows are 30 px tall

## Requirements

- Windows 10+ (Windows only; uses WM_DEVICECHANGE broadcasts, ShellExecuteEx, registry and other
  Win32 APIs)
- Rust 1.95+ (edition 2024)
- usbipd-win 4.4.0 or later

## Build

```powershell
cargo build --release
```

The artifact is written to `target\release\usbip-device-manager.exe`. To run
in debug mode:

```powershell
cargo run
```

## Project Layout

```text
src/
  main.rs      Entry point: single instance, CJK fonts, window options
  app.rs       App state, background tasks, action dispatch
  ui.rs        egui UI rendering
  usbipd.rs    usbipd-win detection, `usbipd state` JSON parsing, bind/attach/detach, daemons
  config.rs    Config read/write (field-compatible with the original config.json)
  sys.rs       Win32 helpers: UAC elevation, registry, network cards, single instance
  monitor.rs   USB plug/unplug broadcast watcher (triggers `usbipd state` diffs)
  lang.rs      Chinese/English strings
  log.rs       File logger
assets/        Icons (copied from the original repository)
```

> Icon copyright belongs to the original WSL USB Manager project (MIT).
