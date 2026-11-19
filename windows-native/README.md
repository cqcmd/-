# 半整点提醒（Windows 原生单文件桌面版）

轻量安静的 Windows 桌面半整点报时与多周期语音闹钟工具（Rust + Slint + Win32 原生接口）。

## 核心特性

- **单文件零依赖**：静态链接 MSVC CRT 并将提示音与高清图标编译入程序，单 `.exe` 体积约 8–12 MB。
- **低占用与资源守护**：Slint 原生矢量渲染（无 WebView2），采用 `AudioBufferPool` 复用音频缓冲区；工作集/私有内存超 `85 MiB` 时自动清理池化缓冲并裁剪工作集，并持续监测连续 5 周期单调内存增长。
- **三套语义主题**：内置「暖金咖啡馆」（默认）、「暗夜曜石」、「晨曦云母」，主窗/设置/关于/Web 控制台实时同步切换，适配 DWM 原生圆角与 PerMonitorV2 高 DPI。
- **本地优先中文语音报时**：默认使用 **Windows 原生 SAPI5（纯离线）**，可选 Edge 在线语音（网络受限自动熔断并零延迟回退本地），支持 `{TIME}`、`{TIME12}`、`{PERIOD}`、`{LABEL}`、`{HH}:{MM}` 变量与 `-5 ~ +5` 语速调节。
- **内置离线节假日历法**：完整内置 2010–2040 公历/农历及传统法定节日识别，无需联网即可自动切换祝福问候与节庆音效。
- **多周期自定义闹钟与静默时段**：支持仅一次、每天、工作日、周末、自定义星期、每隔 N 天；闹钟响铃弹出置顶卡片（关闭 / 稍后 5 分钟），支持跨日静默区间、全链路淡入淡出及 WASAPI 背景音量压低。
- **国内多源容灾 SNTP 校时**：默认 `ntp.aliyun.com` 并自动容灾国内公共授时节点，支持多地址及内网 NTP，按需 UAC 提权写入系统时间。

## 版本规范

- **发行出品**：`Z.W.&AI`
- **版本规则**：`3段式+日期`（`build_version.txt`，如 `2.0.7+20261118`），每段满 10 进位。每次编译时 `build.rs` 自动读取或递增版本号写入程序属性；手动同步可执行 `python bump-version.py`。
- **图标资源**：多分辨率 `assets/app.ico`（16–256px）、`assets/app_icon_512.png` 与托盘图标 `assets/tray_icon_32.rgba`。

## 自动更新与清单格式

默认启用 **Gitee 国内直连源（`djjz/HalfHourChime`）**，支持以下更新源格式（不自动跨网回退 GitHub，无需代理）：

1. **Gitee / GitHub Releases 地址**：自动解析页面/API 获取最新版本号与 `.exe` 下载地址。
2. **标准 `version.json` HTTP(S) 直链**：
```json
{
  "version": "2.0.8+20261119",
  "download_url": "https://your-server.example.com/HalfHourChime.exe",
  "package_type": "portable",
  "notes": "稳定性与交互优化"
}
```

> `package_type` 默认 `"portable"`，通过纯原生 Win32 改名与提权自更新原地替换当前 `.exe` 并无缝重启，不弹出控制台黑框。

## 日志与环境参数

- **日志目录**：优先写入 `config.json` 同级 `logs\`；目录只读时依次回退 `%APPDATA%\HalfHourChime\logs\`、`%TEMP%\HalfHourChime\logs\`。
- **按天归档**：`half_hour_chime_YYYY-MM-DD.log` 默认保留 `14` 天；单日超过 `2 MiB` 自动切分轮转。
- **调试与渲染变量**：
  - `HALF_HOUR_CHIME_DEBUG=1`：开启 `DEBUG` 级别详细日志（含运行时对象引用与 5 周期内存监测详情）。
  - `SLINT_BACKEND=winit-software`：在特定显卡驱动下切换软件光栅渲染。

## 本地编译（Windows）

1. 安装 **Visual Studio 2022 生成工具**（C++ 桌面开发 + Windows 10/11 SDK）与 **Rust 工具链**（`x86_64-pc-windows-msvc`）。
2. 在 `windows-native` 目录运行 `build-release.bat`，产物输出至 `dist\HalfHourChime.exe`。
3. 调试启动直接显示主窗体可执行 `cargo run -- --show`。

## 许可说明

- 应用代码版权归 **Z.W.&AI** 所有；Slint 框架遵循其 Ambassador License / GPLv3 条款。
