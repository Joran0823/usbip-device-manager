# usbip-device-manager

[简体中文](README.md) · [English](README.en.md)

用 **Rust + egui** 重写的 [WSL USB Manager](https://github.com/zcj20080882/wsl-usb-manager)（原实现为 .NET WPF），用于通过 [usbipd-win](https://github.com/dorssel/usbipd-win) 把 Windows 上的 USB 设备绑定/附加到 WSL。

📖 设计文档 / Design docs：[中文](docs/design.md) · [English](docs/design.en.md)

## 功能

- 三个页面：已连接设备（Device）、持久化设备（Persisted）、自动附加列表（Auto Attach）
- 表格列：自动附加、Bus ID、Hardware ID、绑定、附加、强制绑定、设备描述
- 勾选框与右键菜单均可执行 绑定/解绑/附加/分离
- 隐藏/显示设备（过滤列表）、添加到/移出自动附加列表
- 选中设备后显示完整设备信息面板
- USB 即插即用监控：设备插入/拔出自动刷新；自动附加设备插入后自动 attach
- 设备列表通过 `usbipd.exe state` 原生 JSON 获取，不依赖 PowerShell（绑定/附加等操作本就直调 usbipd.exe）
- 指定网卡（`--host-ip`）、使用 BusID、关闭到托盘等设置
- 中文/English 切换、亮/暗主题
- 配置与日志兼容原程序：`%APPDATA%\WSL USB Manager\config.json`、`Logs\yyyyMMdd.log`
- 绑定/解绑自动以管理员权限运行（UAC 提示）

## Roadmap

以下为后续规划方向，当前版本均未实现：

1. **USB/IP 客户端模式（可运行于带桌面环境的虚拟机）**：支持搜索
   USB/IP server 上共享的设备（如运行 usbipd-win 的 Windows 宿主机）并
   支持附加，使本程序可运行在带有桌面环境的虚拟机中，把宿主机共享的
   USB 设备附加进虚拟机。
2. **SSH 访问 Windows 上的虚拟机并附加设备**：支持通过 SSH 访问 Windows
   上的虚拟机，并将 USB 设备附加到该虚拟机中。

## 主题与字体

- `src/theme.rs` 实现深/亮两套完整 `Visuals + Style`，圆角控件、统一字号与配色
- 内嵌 Noto Sans CJK SC 字体（约 15 MB），中英文显示一致、不依赖系统字体
- 全局正文/按钮 15px、辅助文字 13px、标题 19px，设备表格行高 30px

## 运行环境

- Windows 10+（仅 Windows；使用 WM_DEVICECHANGE 广播、ShellExecuteEx、注册表等 Win32 API）
- Rust 1.95+（edition 2024）
- usbipd-win 4.4.0 或更高版本

## 构建

```powershell
cargo build --release
```

产物位于 `target\release\usbip-device-manager.exe`。调试运行：

```powershell
cargo run
```

## 工程结构

```text
src/
  main.rs      入口：单实例、CJK 字体、窗口选项
  app.rs       应用状态、后台任务、操作分发
  ui.rs        egui 界面渲染
  usbipd.rs    usbipd-win 检测、usbipd state JSON 解析、绑定/附加/分离、守护进程
  config.rs    配置读写（与原 config.json 字段兼容）
  sys.rs       Win32 帮助：UAC 提权、注册表、网卡、单实例
  monitor.rs   USB 插拔广播监听（触发 usbipd state 差集）
  lang.rs      中英文文案
  log.rs       文件日志
assets/       图标（从原仓库复制）
```

> 图标版权归原 WSL USB Manager 项目（MIT）。
