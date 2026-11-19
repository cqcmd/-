# 半整点提醒（HalfHourChime）

轻量、纯本地运行的 **Windows 原生单文件定时报时与多周期语音闹钟工具**（Rust + Slint + Win32 原生接口），配套提供基于 Web 的工程源码与运行监控工作台。

## 核心特性

- **单文件零依赖**：静态链接 MSVC CRT（`+crt-static`），内嵌音频与高清图标资源，双击即可驻留系统托盘，无需 WebView2 或额外运行库。
- **低占与内存泄漏守护**：Slint 原生矢量渲染，内置 PDH / PSAPI 监控与 `AudioBufferPool` 音频缓冲池；工作集或私有内存超 `85 MiB` 自动压缩堆并裁剪工作集，并支持连续 5 周期内存单调增长告警与对象引用追踪。
- **本地优先中文语音报时**：默认使用 **Windows 本地 SAPI5（纯离线）**，可选 Edge 在线语音（异常自动熔断回退本地），支持半/整点真实时间播报及 `-5 ~ +5` 语速调节。
- **离线公历/农历节假日**：内置 2010–2040 年农历算法与法定/传统节日库，零外部接口即可自动切换节日祝福语与专属提示音。
- **灵活报时与多组自定义闹钟**：整点 (`:00`)、半点 (`:30`) 独立开关，可全取消进入纯闹钟模式；支持仅一次、每天、工作日、周末、自定义星期、每隔 N 天等周期，配独立置顶提醒弹窗（关闭 / 稍后 5 分钟）。
- **背景音平滑压低**：触发提示音或语音播报时自动平滑淡入淡出，并通过 WASAPI 临时压低背景播放音量。
- **国内多源容灾校时与国内更新**：RFC 4330 SNTP 校时默认优先 `ntp.aliyun.com` 并自动容灾国内公共授时源（支持内网 NTP）；更新默认直连 **Gitee Releases**，也兼容标准 `version.json`。
- **多主题无边框圆角界面**：内置「暖金咖啡馆」（默认）、「暗夜曜石」、「晨曦云母」三套主题，适配 Windows 10/11 原生圆角与高 DPI。
- **托盘与本地控制台**：**左键单击托盘图标**弹出主窗口，右键提供快捷控制、静音模式、自启、打开日志目录、检查更新及可选本地 Web 控制台（3000 端口）。

## 目录结构

```text
.
├── windows-native/                 # Windows 原生桌面端源码（Rust + Slint 1.8）
│   ├── src/
│   │   ├── main.rs                 # 单实例、托盘交互、250ms 调度器、自动更新与窗口管理
│   │   ├── audio.rs                # 音频解码、Buffer 池、淡入淡出、WASAPI 压低、SAPI5/Edge TTS
│   │   ├── scheduler.rs            # 整半点/闹钟调度、跨日静默、农历节日、5 周期泄漏监控
│   │   ├── config.rs               # JSON 持久化与配置迁移校验
│   │   ├── ntp.rs                  # 国内容灾 SNTP 客户端与 UAC 系统校时
│   │   ├── version.rs              # 版本号比对与 Gitee/本地/LAN/HTTP 清单解析
│   │   ├── web_console.rs          # 可选本地 3000 端口 Web 控制台
│   │   └── win_sys.rs              # 托盘、注册表自启、DWM 圆角、SAPI5、按天日志归档
│   ├── ui/                         # Slint 多主题界面定义
│   ├── build-release.bat           # Windows 本地测试与 Release 打包脚本
│   ├── README.md                   # 原生端技术架构与日志/更新说明
│   ├── USER_MANUAL.md              # 使用说明书
│   └── ACCEPTANCE_MATRIX.md        # 验收核对矩阵
├── .github/workflows/
│   └── release-windows.yml         # 自动编译、发布 GitHub Releases 并同步 Gitee
└── src/                            # Web 源码浏览与运行监控分析工作台
```

## 快速使用

1. 前往 **[Releases](../../releases)** 下载 `HalfHourChime.exe`。
2. 双击运行后驻留右下角托盘，**左键单击托盘图标**弹出主窗口，右键打开快捷菜单。
3. 详细功能说明见 [使用说明书](windows-native/USER_MANUAL.md)。

## 编译与构建

### 云端自动发布

仓库内置 [Windows Release Build & Publish](.github/workflows/release-windows.yml)：推送 `v*` 标签或手动触发工作流时，自动在 `windows-latest` 上编译 `HalfHourChime.exe` 与 `version.json` 并同步发布到 GitHub 与 Gitee Releases。

### Windows 本地编译

安装 **Visual Studio 2022 生成工具**（C++ 桌面开发 + Windows 10/11 SDK）与 **Rust 工具链**（`x86_64-pc-windows-msvc`），在 `windows-native` 目录执行：

```bat
build-release.bat
```

产物输出至 `windows-native\dist\HalfHourChime.exe`。

### 启动 Web 工作台

```bash
npm install
npm run dev
```

## 版权与许可

- 本项目应用代码由 **Z.W.&AI** 出品。
- UI 框架 Slint 采用免版税桌面商业授权（Ambassador License）及 GPLv3 开源协议；其余依赖遵循各自开源协议。
