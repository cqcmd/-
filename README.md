# 半整点提醒（HalfHourChime）

面向网吧、电竞酒店与桌面集中值守场景的 **Windows 原生单文件半整点报时与语音提醒工具**（Rust + Slint + Win32 原生接口），配套提供基于 Web 的工程源码浏览与运维分析工作台。

---

## 核心特性

- **真单文件、零依赖运行**：静态链接 MSVC CRT（`+crt-static`），将提示音与图标内嵌至单一 `HalfHourChime.exe`，无需安装 WebView2 或 VC++ 运行库，双击即驻留系统托盘。
- **极低资源占用与自动内存守护**：采用 Slint 原生界面，内置 Windows PDH / PSAPI 监控，工作集或私有提交内存超过 `50 MiB` 时自动清理空闲缓冲并回收工作集，低配终端长期运行稳定。
- **平滑音频过渡与背景音自动压低**：全链路播放支持平滑淡入淡出，触发餐饮/定时提醒时通过 Windows WASAPI 自动压低其他媒体播放器的背景音量，播报结束后平滑恢复。
- **真实中文时间语音报时（双引擎）**：支持 **Windows 本地 SAPI5（离线可用）** 与 **Microsoft Edge TTS（在线神经网络语音，异常自动回退 SAPI5）**，支持半/整点真实中文时间播报与 `-5 ~ 5` 语速调节。
- **农历与节假日专属报时**：内置 2010–2040 年农历转换及法定/传统/电竞节点库，支持自定义节日规则与外部 JSON 数据源；开启后会在已设定的报时分钟（默认 `:00` / `:30`）自动切换专属提示音与祝福问候。
- **多主题无边框圆角界面**：内置「暖金咖啡馆」（默认）、「暗夜曜石」、「晨曦云母」三套高对比主题，支持实时预览切换与 Windows 10/11 圆角适配。
- **完整运维辅助能力**：
  - 托盘右键快捷控制：**静音模式**、**检查更新**、**快速访问日志文件夹**、**测试报时**、**立即校时**、**开机自启**。
  - **跨日静默时段**（如 `23:00–07:00`）与**餐饮自定义音频提醒**（支持 `WAV / MP3 / FLAC / OGG`）。
  - **RFC 4330 SNTP 网络校时**与按需 UAC 提权写入系统时间。
  - **按天归档日志**（`logs/half_hour_chime_YYYY-MM-DD.log`，保留 14 天、单日按大小轮转）。
  - **多源自动更新**：兼容标准 `version.json`、GitHub Releases 与局域网共享路径。

---

## 仓库目录结构

```text
.
├── windows-native/                 # Windows 原生桌面端源码（Rust + Slint 1.8）
│   ├── src/
│   │   ├── main.rs                 # 单实例、托盘循环、250ms 调度器、自动更新与内存守护
│   │   ├── audio.rs                # 音频解码、淡入淡出、WASAPI 背景压低、SAPI5/Edge TTS
│   │   ├── scheduler.rs            # 整半点调度、跨日静默、农历与节假日规则引擎
│   │   ├── config.rs               # JSON 持久化与配置校验
│   │   ├── ntp.rs                  # SNTP 客户端与 UAC 提权同步系统时间
│   │   ├── version.rs              # 3段式+日期版本号与更新清单解析
│   │   └── win_sys.rs              # 托盘菜单、注册表自启、DWM 圆角、按天日志归档
│   ├── ui/                         # Slint 界面与三套语义主题定义
│   ├── build-release.bat           # Windows 本地一键单元测试与 Release 打包脚本
│   ├── README.md                   # Windows 原生端技术与日志/环境变量说明
│   ├── USER_MANUAL.md              # 门店/网吧运维快速上手与批量部署手册
│   └── ACCEPTANCE_MATRIX.md        # 功能与验收核对矩阵
├── .github/workflows/
│   └── release-windows.yml         # GitHub Actions 云端自动测试、编译与发布 Release
└── src/                            # 配套 Web 工程工作台（错误监控报告 / 系统开销分析 / 源码查看）
```

---

## 下载与使用

### 方式一：直接下载成品（推荐）

1. 前往本仓库右侧的 **[Releases](../../releases)** 页面下载最新的 `HalfHourChime.exe`。
2. 放到任意目录后双击运行，程序默认最小化驻留在系统右下角托盘（右键或双击图标打开主面板与设置）。
3. 如需开启自动检查更新，可在「设置 → 后台定期检查版本更新 → 更新接口地址」填入本仓库 Releases 地址：
   ```text
   https://github.com/<用户名>/<仓库名>/releases/latest
   ```
4. 门店/网吧批量部署与日常运维细节请参考：[运维使用手册](windows-native/USER_MANUAL.md)。

---

## 编译与发布

### 1. GitHub Actions 云端自动编译发布（无需本地配环境）

仓库已内置 [Windows Release Build & Publish](.github/workflows/release-windows.yml) 工作流：

- 推送代码至 `main` / `master` 分支时，自动在 `windows-latest` 上执行 Rust 单元测试并编译 `HalfHourChime.exe` 构建产物。
- 推送 `v*` 标签，或在仓库 **Actions → Windows Release Build & Publish → Run workflow** 手动触发时，将自动编译生成 `HalfHourChime.exe` 与 `version.json` 并发布至 **GitHub Releases**。

### 2. Windows 本地编译

需安装 **Visual Studio 2022 生成工具**（C++ 桌面开发 + Windows 10/11 SDK）与 **Rust 工具链**（`x86_64-pc-windows-msvc`），进入 `windows-native` 目录执行：

```bat
build-release.bat
```

编译完成后即可在 `windows-native\dist\HalfHourChime.exe` 获得单文件成品。

### 3. 启动配套 Web 运维工作台

```bash
npm install
npm run dev
```

工作台默认运行在 `http://localhost:3000`，提供源码浏览、7 天日志错误监控分析与系统开销趋势分析面板。

---

## 版权与许可

- 本项目应用代码由 **Z.W.&AI** 出品。
- 桌面端 UI 框架 Slint 采用免版税桌面商业授权（Ambassador License）及 GPLv3 开源协议；第三方 Rust 依赖遵循各自开源协议。
