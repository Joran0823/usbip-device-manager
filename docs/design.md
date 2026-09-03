# usbip-device-manager 设计文档

> 适用版本：v0.1.0（`usbip-device-manager.exe`）
> 展示名称：USBIP Device Manager / USBIP 设备管理器
> 最后更新：2026-09-03
> Language: [中文](design.md) · [English](design.en.md)

本文记录 `usbip-device-manager` 的整体设计：架构、线程模型、UI 布局、
usbipd-win 交互以及关键设计决策，便于后续维护和评审。

## 1. 背景与目标

本项目是用 **Rust + egui** 重写原 .NET WPF 版
[wsl-usb-manager](https://github.com/zcj20080882/wsl-usb-manager) 的桌面程序，
通过 [usbipd-win](https://github.com/dorssel/usbipd-win) 把 Windows 上的
USB 设备绑定（bind）并附加（attach）到 WSL 2。

设计目标：

- 功能与交互对齐原 C# 程序：三个页面、勾选框与右键菜单两种操作入口、
  隐藏设备、自动附加、托盘、配置与日志目录兼容。
- 不依赖 .NET / PowerShell 运行时；设备列表直连 `usbipd.exe state` 原生 JSON，
  降低每次刷新/插拔响应的延迟。
- UI 提供深/亮双主题、统一字号与配色，内嵌 CJK 字体，中英文显示一致。
- 保留与原程序共享的 `%APPDATA%\WSL USB Manager` 配置目录，
  老用户升级不丢配置。

## 2. 技术选型与依赖

| 依赖 | 用途 |
| --- | --- |
| `eframe` / `egui` 0.36（glow） | 即时模式 GUI、窗口、视图层 |
| `tray-icon` 0.24 | 系统托盘图标与菜单 |
| `image` | 加载/缩放托盘与窗口图标 |
| `serde` / `serde_json` | `config.json` 与 `usbipd state` JSON 解析 |
| `winreg` | 注册表 Uninstall 键查找 usbipd-win 安装信息 |
| `windows-sys` 0.61 | SetupAPI、ShellExecuteEx、注册表、网卡、locale 等 Win32 API |
| `embed-resource`（build） | 把 `appicon.ico` 嵌入可执行文件资源 |

仅支持 Windows 10+；Rust edition 2024。无 .NET / PowerShell 依赖。

## 3. 目录结构与模块职责

```text
src/
  main.rs      入口：单实例、CJK 字体、窗口选项、eframe 启动
  app.rs       应用状态、后台线程、消息分发、Action 处理、托盘
  ui.rs        egui 渲染：页面/列表/右键菜单/信息区/设置/弹窗/toast
  usbipd.rs    usbipd-win 检测、usbipd state JSON 解析、绑定/附加/守护进程
  config.rs    配置结构（PascalCase JSON，兼容原 C# 程序）
  sys.rs       Win32 封装：提权、注册表、网卡、SetupAPI 枚举、单实例、locale
  monitor.rs   USB 插拔轮询监控线程
  lang.rs      中英文文案表与当前语言状态
  theme.rs     深/亮主题
  log.rs       文件日志
assets/
  appicon.ico  可执行文件图标（build.rs 嵌入）
  appicon.png  窗口图标与托盘图标（运行时加载）
  fonts/       Noto Sans CJK SC（include_bytes 内嵌）
```

| 模块 | 主要职责 |
| --- | --- |
| `main.rs` | 日志初始化、单实例互斥、构建 viewport、设置字体、启动 eframe |
| `app.rs` | `App` 状态；`UiMsg` 消息处理；`Action` 分发；后台线程调度；托盘构建 |
| `ui.rs` | 每帧渲染；页面布局与各区域独立滚动；右键菜单；弹窗与 toast |
| `usbipd.rs` | `Usbipd::check()`、`list_devices()`（JSON）、bind/unbind/attach/detach、`DaemonManager` |
| `sys.rs` | `run_elevated`、`find_installed_app`、`network_cards`、`usb_hardware_ids`、单实例、locale |
| `monitor.rs` | 250ms 轮询 SetupAPI 枚举，diff 后发出插/拔事件 |
| `config.rs` | `SystemConfig`/`AppConfig`/`UsbDevice` 与读写、路径 |
| `theme.rs` | 深/亮两套 `Visuals`/`Style` 与主题感知取色 |
| `lang.rs` | 文案 key → 中/英文；`lang::t()`；启动时语言检测 |
| `log.rs` | `%APPDATA%\WSL USB Manager\Logs\yyyyMMdd.log` 追加日志 |

## 4. 启动流程与生命周期

1. `log::init()`：建日志目录，写入启动标记。
2. `sys::acquire_single_instance()`：命名互斥体（
   `usbip-device-manager-<guid>`）。若已存在则弹提示并退出，
   避免双实例同时操作 usbipd。
3. 读取 `config.json`；若未配置语言，按系统 locale（`GetUserDefaultLocaleName`
   是否以 `zh` 开头）决定初始语言。
4. 创建 eframe 窗口（标题栏图标 = 托盘图标），启动：
   - `usbipd-check` 线程：`Usbipd::check()` 检测安装与版本；
   - `UsbMonitor`：开始 250ms 的 USB 枚举轮询；
   - 托盘图标与菜单事件线程。
5. 收到 `UiMsg::UsbipdReady` 后进入就绪状态并触发首次列表刷新。
6. 退出：点击 Exit / 关闭窗口（关闭到托盘时改为隐藏）；`App::drop` 停止
   auto-attach 守护进程、monitor 与托盘。

## 5. UI 设计

### 5.1 整体布局

- 顶栏：左侧三个页面标签（Device / Persisted / Auto Attach），
  右侧依次为 Refresh、主题切换（Light/Dark）、语言菜单（中文 / English）、
  Settings、Exit；busy 时显示 spinner 并禁用刷新。
- 切换标签页会触发一次刷新，并强制回到“仅显示可见设备”
  （`show_filtered = false`），保证隐藏设备在刷新后保持隐藏。
- 语言、主题、隐藏过滤等状态以 `Action` 形式收集，在当前 UI 帧结束后统一
  执行，避免渲染中修改状态。

### 5.2 页面与滚动策略

三个页面各自管理滚动区域（不滚动整个窗口主体）：

| 页面 | 列表 | 信息区 |
| --- | --- | --- |
| Device | 高度自适应，上限 = 窗口可用高度的 2/3（有选中设备时再预留约 100px） | 占据剩余高度，滚动条自动 |
| Persisted | 同上（2/3 上限 + 信息区预留） | 同上 |
| Auto Attach | 占满剩余高度，滚动条自动 | 无 |

- 列表高度小于上限时不出现滚动条（`max_height` 限制而非固定高度）；
  超过上限才出现，因此列表不会撑开窗口，剩余空间全部留给信息区。
- 每个滚动区域使用固定 `id_salt`（如 `devices-list`、`devices-info`），
  切换页面/刷新后保留各自的滚动位置。
- 设备表格行高 30px；表头与条目同一字号并加粗；文本单元截断显示。
- 右键菜单（Device 页）：Bind / Unbind、Attach / Detach、
  Hide / Show / 显示隐藏切换、Add to Auto / Remove from Auto，
  并按设备状态禁用对应项；右键也同时选中该行。
- 设备信息区只读展示 InstanceId、HardwareId、BusId、PersistedGuid、
  IsBound/IsConnected/IsAttached、ClientIPAddress 等，内容超长时可滚动。

### 5.3 主题

主题方案（`theme.rs`）：

- 深/亮两套完整 `Visuals + Style`，常量定义背景、面板、输入框、按钮、
  文字、边框等颜色，控件统一圆角（6px）与字号；
- 强调色（蓝）、错误红、成功绿全局一致；
- 主题状态用 `thread_local` 保存（egui 渲染在单线程），`is_dark()` /
  `text()` 等取色函数贯穿 UI；
- 亮/暗主题切换即改配置、写盘并重新 `apply_theme`。

### 5.4 字体与图标

- `main.rs` 将 Noto Sans CJK SC（约 16MB）`include_bytes` 内嵌，
  插入 Proportional 字体首位、Monospace 兜底，中英文不依赖系统字体。
- `build.rs` 用 `embed-resource` 把 `appicon.ico` 编入 exe 资源
  （Explorer/任务栏图标）；
- 运行时用 `assets/appicon.png` 分别生成 64x64 窗口标题栏图标与
  32x32 托盘图标，两处使用同一张图标。

### 5.5 语言

- 顶栏语言按钮为弹出菜单（中文 / English），不再单独放两个切换按钮；
- `lang.rs` 用 key → 中/英文文案表；`lang::t()` 按全局语言状态取值；
- 语言随配置持久化；配置为空时按系统 locale 决定，切换语言时同步更新
  托盘菜单文字（保存 `MenuItem` 句柄后 `set_text`）。

### 5.6 托盘

- 托盘菜单：Show / Exit；关闭窗口且配置了“关闭到托盘”时隐藏窗口而非退出。
- 托盘事件在独立 `tray-menu` 线程监听，通过 `UiMsg` 回到 UI 线程处理，
  并 `request_repaint()` 唤醒渲染。

## 6. 配置与数据模型

`config.rs` 的结构与 JSON 字段保持原 C# 程序的 PascalCase 布局，
配置文件位于 `%APPDATA%\WSL USB Manager\config.json`（刻意不与新名绑定，
以兼容旧版）。

```json
{
  "AppConfig": { "DarkMode": false, "Lang": "zh", "CloseToTray": true,
                 "UseBusID": false, "SpecifyNetCard": false,
                 "ForwardNetCard": "" },
  "AutoAttachDeviceList": [ { "HardwareId": "...", ... } ],
  "FilteredDeviceList": []
}
```

`UsbDevice` 是核心设备模型：

| 字段 | 含义 | 主要来源 |
| --- | --- | --- |
| `instance_id` | Windows 设备实例 ID | usbipd state JSON |
| `hardware_id` | `VID:PID`（小写 hex，如 `048d:5702`） | 由 InstanceId 提取 |
| `description` | 设备描述 | JSON |
| `is_forced` | 是否强制绑定 | JSON |
| `bus_id` | 总线 ID（连接时才有） | JSON |
| `persisted_guid` | 绑定持久化 GUID（已绑定时才有） | JSON |
| `stub_instance_id` | usbip 桩设备实例 | JSON |
| `client_ip_address` | 附加到的客户端 IP（已附加时才有） | JSON |
| `is_bound` / `is_connected` / `is_attached` | 派生状态 | 由 GUID/BusId/IP 推导 |

- `AutoAttachDeviceList`、`FilteredDeviceList` 以 `hardware_id` 为键，
  比较时忽略大小写；
- 勾选“Auto Attach”会立即执行绑定 + 附加（见 7.5），并写入自动附加列表；
- “隐藏”写入过滤列表，右键“显示”或“显示隐藏”从列表移除/临时显示。

## 7. usbipd-win 交互设计

### 7.1 安装检测与版本

- `sys::find_installed_app("usbipd-win")` 遍历 HKLM/HKCU 的 Uninstall 键
  （含 Wow6432Node），匹配 DisplayName 得到 InstallLocation 与版本；
- 要求 `usbipd.exe` 存在且版本 ≥ 4.4.0（`major.minor` 比较），否则提示
  升级/安装后重启程序；
- 安装目录不在本地盘符时给出警告（远程磁盘上的 usbipd 不可用）。

### 7.2 设备列表：原生 `usbipd state` JSON（方案 A）

早期版本通过 PowerShell `Import-Module …; Get-UsbipdDevice` 取列表，
每次刷新/插拔都要冷启动 PowerShell，明显增加延迟。现改为直接执行：

```text
usbipd.exe state
```

输出 JSON（usbipd-win ≥ 2.2.0 起支持，本项目要求 ≥ 4.4.0 满足）；
`Get-UsbipdDevice` 本身只是对该 JSON 的再包装。实现要点：

- `serde` 按官方字段名反序列化，注意 `ClientIPAddress` 需显式 `rename`；
- 空 InstanceId 的设备直接过滤；
- `IsBound/IsConnected/IsAttached` 不在 JSON 中，由
  `PersistedGuid/BusId/ClientIPAddress` 是否非空推导；
- `hardware_id` 从 InstanceId 提取 `VID_xxxx&PID_xxxx`（大小写不敏感、
  恰 4 位 hex、PID 后不能紧跟 hex 数字），统一输出小写 `vid:pid`——
  与 PowerShell 模块显示值及 `usbipd bind --hardware-id` 的参数格式一致
  （官方 `VidPid.TryParseId` 同规则）；
- stdout 为 UTF-8，中文描述直接解析，无乱码；8 秒超时，非 0 退出码报
  stderr。

本机实测：`usbipd.exe state` 平均约 61ms，旧 PowerShell 方案约 276ms，
单次查询快约 4.5 倍；刷新、USB 插拔后的刷新及绑定/附加后的校验轮询
全部复用该路径。

### 7.3 bind / unbind：提权设计

绑定/解绑必须写注册表/驱动相关状态，需要管理员权限。当前实现：

- `usbipd.exe bind|unbind --hardware-id|--busid …` 通过
  `sys::run_elevated()` 执行；
- `run_elevated` 使用 `ShellExecuteExW` + `runas` verb 弹出标准 UAC 提示，
  隐藏窗口等待退出码，10 秒超时；用户取消（`ERROR_CANCELLED`）时返回
  “用户取消了管理员提示”；
- bind 默认加 `--force` 选项按设备状态传入；操作后轮询 `state` 确认
  状态再返回成功。

限制：在企业级特权管理（EPM）接管 UAC 的环境中，`runas` 是否成功取决于
管控策略，程序无法绕过。改进方向（未实现）：检测当前进程是否已提权；
若已提权则直接以当前 token 启动 `usbipd.exe`，不再二次触发 UAC/EPM。

### 7.4 attach / detach

- attach：`usbipd attach (--busid|--hardware-id) --wsl [--host-ip …]`
  （无需提权）；解析常见错误（防火墙、无 WSL 运行、设备被 Windows 占用）
  后翻译为本地文案；成功后轮询确认 `IsAttached`。
- detach：`usbipd detach …`，轮询确认后停止对应 auto-attach 守护进程。
- 附加前若选择网卡，则 `attach --host-ip <IPv4>`；网卡列表由
  `GetAdaptersAddresses` 枚举（剔除回环/隧道，仅返回运行中的 IPv4）。

### 7.5 Auto Attach

- 勾选自动附加即触发“绑定（如未绑定）+ sleep 1s + attach”；
- 成功后再启动一个 `usbipd attach … --auto-attach` 长驻进程
  （`DaemonManager` 记录 PID 与参数，用 `is_process_running` 清理僵尸、
  `taskkill /T /F` 停止），让 usbipd 持续监听设备拔出/重插并自动回连；
- 程序每帧空闲时对“已连接、自动附加、未附加且最近 8 秒内未触发”的设备
  执行自动附加；Device 页行内勾选框与右键菜单均可操作。

### 7.6 操作后一致性校验

绑定/解绑/附加/分离后都通过 `list_devices()`（`usbipd state`）轮询校验，
而不是只信命令退出码：

- bind：最多 3 轮、每轮 500ms 确认 `IsBound`；
- attach：最多 10 轮、每轮 500ms 确认 `IsAttached`；
- detach：同样 10 轮确认 `!IsAttached`。

## 8. USB 热插拔监控

`monitor.rs` + `sys::usb_hardware_ids()` 组成轮询式监控：

1. 每 250ms 用 SetupAPI（class=USB、DIGCF_PRESENT|ALLCLASSES）枚举当前
   USB 设备 InstanceId，提取大写 `VID:PID` 集合（排序去重）；
2. 与上一轮集合求差：新增 → `connected=true`，消失 → `connected=false`，
   通过 `UiMsg::UsbChanged` 上报并 `request_repaint`；
3. UI 帧内 `queue_usb_change` 先合并事件：忙/正在列表时不处理；距上次
   刷新不足 900ms 视为系统仍在枚举，继续合并等待；
4. 冷却结束后由 `usb-event` 线程执行一次 `list_devices()`，刷新三页数据，
   并对变化设备发状态 toast；`maybe_auto_attach` 随后自动附加匹配设备。

曾经的“插入约 7 秒后才显示”主要是两个因素叠加：

- Windows 对复合/多接口设备是逐个枚举的，事件在数秒内高频出现；
- 旧实现每次事件都冷启动 PowerShell（约 276ms/次），事件风暴造成连续
  启动多个 PowerShell 并互相竞争。

现方案合并去抖 + 单次原生 `usbipd state`（约 60ms/次）后，插拔只触发
一次刷新，延迟明显下降。

## 9. 线程模型与消息机制

egui 是单线程渲染模型，所有耗时工作放在后台线程，结果经
`std::sync::mpsc` 发回 UI 线程（每帧 `try_recv` 批量处理）。

| 线程 | 职责 | 触发 |
| --- | --- | --- |
| UI 主线程（`logic`/`ui`） | 渲染、收消息、flush USB 事件、分发 Action | 常驻 |
| `usbipd-check` | 检测安装/版本 | App 启动一次 |
| `refresh` | `list_devices()` 刷新 | 手动刷新/切页/初始化 |
| `usbipd-op` | bind/unbind/attach/detach 等一次操作 | 每次用户操作 |
| `usb-event` | 插拔冷却后的列表刷新 | 每次合并后的插拔事件 |
| `usb-monitor` | 250ms SetupAPI 轮询 | App 生命周期内 |
| `tray-menu` | 托盘菜单事件 | 托盘存在期间 |

核心消息（`UiMsg`）：

| 消息 | 含义与处理 |
| --- | --- |
| `UsbipdReady` | usbipd 检查完成：置就绪、触发首次刷新 |
| `Lists(Vec<UsbDevice>)` | 刷新结果：重建三页数据、恢复选中、自动附加 |
| `UsbListFinished` | 列表任务结束：清 busy |
| `OpDone` | 操作结果：toast/错误弹窗，然后刷新 |
| `Error` | 错误：写日志 + 弹窗 |
| `UsbChanged` | 插拔事件：合并去抖 |
| `UsbNotify` | 插拔后设备状态 toast |
| `ShowWindow` / `ExitApp` | 托盘控制 |

忙状态通过 `busy` 标志串联：操作中继续点刷新只置 `need_refresh`，
等当前任务结束后补刷新，避免并发调用 usbipd；busy 期间每 120ms
主动重绘（spinner）。

## 10. 错误处理与日志

- 用户操作错误以模态弹窗展示（`Dialog`），短暂成功/信息用 toast；
- 所有后台失败统一 `UiMsg::Error` 回 UI 写日志并弹窗；
- 日志写入 `%APPDATA%\WSL USB Manager\Logs\yyyyMMdd.log`，
  行格式含时间、线程名、级别；设置页可清空日志/打开目录；
- 关键路径均有超时（进程 8–10 秒）与退出码分类（
  `ExitCodeToErrCode`：0 成功、2/3 未找到、5/126 拒绝访问等）。

## 11. 构建、测试与发布

```powershell
cargo test          # 单元测试：JSON 解析、版本解析、配置 PascalCase、主题
cargo build --release
```

产物：`target\release\usbip-device-manager.exe`。

测试覆盖重点：

- `usbipd state` JSON：null 字段、空 InstanceId 过滤、VID/PID 大小写与
  位数边界、派生标志；
- 旧 PowerShell 文本解析保留为 `#[cfg(test)]` 参考测试；
- 版本解析（`4.4.0`、`v5.2.1-preview`）；
- config.json 与原 C# PascalCase 形状互相兼容；
- 主题两套取色可读性。

## 12. 已知限制与后续优化

- bind/unbind 依赖 UAC/EPM 放行（见 7.3）；后续可加“进程已提权则直跑
  usbipd”检测；
- USB 监控为 250ms 轮询而非事件驱动；可改用 SetupAPI 注册设备通知以
  消除轮询间隔，成本低、收益有限；
- 单实例、单 usbipd 工作流是刻意取舍，避免并发命令互相干扰；
- 配置目录继续使用 `WSL USB Manager` 名称以兼容旧版，换新名会丢配置；
- 若未来需要兼容 4.4.0 之前的 usbipd（无 `state` JSON），可保留
  PowerShell 解析作为降级路径（当前刻意未实现）。
