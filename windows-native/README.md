# 半整点提醒（Windows 原生单文件桌面版）

轻量安静的 Windows 桌面半整点报时与定时语音提醒工具（Rust + Slint + Win32 原生接口）。

## 核心特性
- **真单文件、零依赖交付**：静态链接 MSVC CRT 并将两套高清提示音（`chime.wav`、`meal.wav`）及托盘图标直接编入可执行文件，单 `.exe` 体积约 8–12 MB，双击即用。
- **极低常驻占用**：原生矢量渲染引擎，不内嵌 WebView2/Chromium；后台常驻托盘内存约 15–25 MB，空闲 CPU 占用接近 0%。
- **三套精致主题（默认「暖金咖啡馆」）**：内置「暗夜曜石」「晨曦云母」「暖金咖啡馆」三套主题，主窗口与设置窗口实时无闪烁切换。
- **圆角无边框定制标题栏与自动居中**：取消系统标准标题栏，采用定制标题栏（保留「最小化」与「关闭」按钮，支持拖拽移动），通过 DWM/GDI 自动保持 Windows 11/10 原生圆角外观，全部窗体启动或弹出时自动居中显示。
- **实时时间语音报时与自定义文字转语音**：调用 Windows 原生 SAPI 5（`ISpVoice`）与 Microsoft Edge TTS 双引擎，支持半/整点播报真实中文时间（如「现在时间，十四点三十分」「十二点整」）以及定时自定义文字转语音（支持 `{TIME}`、`{TIME12}`、`{PERIOD}`、`{HH}:{MM}` 占位符与 `-5~5` 语速调节），并可与提示音/自定义音频串联播报。
- **农历与节假日专属报时（节庆氛围）**：内置 2010–2040 农历转换及法定/传统/电竞节庆库（除夕、春节、元宵、清明、端午、七夕、中秋、重阳、元旦、五一、国庆及 520/双十一/平安夜/圣诞等节点），支持本地规则与外部 HTTP(S)/局域网 JSON 节假日数据接口，命中节庆时自动切换专属报时音与祝福问候语（支持 `{HOLIDAY}`、`{GREETING}`、`{LUNAR}`、`{TIME}`）。
- **完整需求覆盖**：整/半点及自定义分钟报时、跨日静默时段（如 `23:00–07:00`）、餐饮提醒（支持浏览本地 `WAV/MP3/OGG/FLAC` 自定义音频与“内置提示音 + 自定义音频”组合播报，触发时通过 WASAPI 自动压低其他背景音乐音量并在播报后平滑恢复）、全链路音频平滑淡入淡出（避免报时切换突兀爆音）、0–100 实时音量试听、RFC 4330 SNTP 校时与按需 UAC 系统时钟同步、单实例唤醒、注册表开机自启。

## 版本号与图标规范
- **版本信息**：`Z.W.&AI`
- **版本号格式与每次编译自动 +1**：`3段式+日期`（`build_version.txt`，如 `2.0.0+20261114`），**每段满 10 进位**（如 `2.0.9 -> 2.1.0`，`2.9.9 -> 3.0.0`）。每次执行 `cargo build` / `cargo run` 时，`build.rs` 会自动读取并递增 `build_version.txt`（+1 并刷新当年今日日期）注入程序与 Win32 文件属性；亦可手动执行 `python bump-version.py` 同步 `Cargo.toml`。
- **高清图标资源**：已配置多分辨率 Windows ICO（`assets/app.ico`，覆盖 `16px–256px`）、窗体高清 PNG（`assets/app_icon_512.png`）与系统托盘图标（`assets/tray_icon_32.rgba`）。

## 后台定期更新检查与自动升级清单格式
程序启动 5 秒后及每隔配置间隔（默认 6 小时）会在后台静默请求配置的更新源地址（`update_check_url`），支持以下 4 类更新源（自动识别 Windows 系统代理）：
1. **标准 `version.json` HTTP(S) 直链**（服务端返回如下 JSON）：
```json
{
  "version": "2.0.1+20261120",
  "download_url": "https://your-server.example.com/HalfHourChime.exe",
  "package_type": "portable",
  "notes": "优化高 DPI 显示与静默时段表现"
}
```
2. **GitHub / Gitee Releases 地址**（如 `https://github.com/你的用户名/仓库/releases/latest` 或 API 地址，自动提取 `tag_name` 与 `.exe` 资产直链）。
3. **Google Drive 公开分享链接**（支持将 `version.json` 或新版 `.exe` 设为「任何知道链接的人可查看」并直接填入分享链接，程序自动转换为直链下载）。
4. **局域网共享或本地路径**（如 `\\192.168.1.250\Updates\version.json` 或 `D:\Updates\version.json`，内网无外网环境亦可平滑自动更新）。

> `package_type` 默认为 `"portable"`（单文件下载至系统临时目录，校验可执行文件头后通过独立分离升级脚本平滑替换当前 `.exe` 并自动重启）；若发布为独立安装向导包，可设为 `"installer"`。

## 调试日志与故障排查
- **日志位置优先级**：优先写入 `config.json` 同级 `logs\` 目录（打包目录 `dist\logs\` 已预设创建）；若所在目录只读（如无盘锁定或 `Program Files`），依次回退至 `%APPDATA%\HalfHourChime\logs\` 与 `%TEMP%\HalfHourChime\logs\`。
- **按天归档与清理**：按 `half_hour_chime_YYYY-MM-DD.log` 命名每日自动归档，默认保留最近 `14` 天运行记录（方便运维按指定日期快速导出排查）；单日超出 `2 MiB` 时额外切分 `.log.1` ～ `.log.3` 防爆盘，采用 Windows `CRLF` 换行可直接用记事本打开。
- **开启详细调试级别**：Release 版默认记录 `INFO` 及以上（启动环境快照、配置警告、NTP 校时、报时触发、自启注册表、更新检查与升级重启）；如需排查细节，可设置环境变量 `HALF_HOUR_CHIME_DEBUG=1` 后启动。
- **低配性能与内存守护**：内置 Windows 原生 PDH 性能计数器（`\Processor(_Total)\% Processor Time`、`\Process(*)\% Processor Time`、`\Process(*)\Working Set - Private`）与 PSAPI/内核时间片双路兜底监控，当工作集或私有提交内存超过 `50 MiB` 时自动清理空闲音频缓冲、紧缩进程堆（`HeapCompact`）并裁剪工作集（`EmptyWorkingSet`）；高 CPU 负载时自动延后非关键后台网络任务。

## 编译环境准备（Windows）
1. 安装 **Visual Studio 2022 生成工具**（勾选“使用 C++ 的桌面开发”与 Windows 10/11 SDK）。
2. 安装 **Rust 工具链**（推荐通过 `rustup-init.exe`，默认选择 `x86_64-pc-windows-msvc`）。
3. 在 `windows-native` 目录下双击或命令行执行 `build-release.bat`，即可在 `dist\HalfHourChime.exe` 获得最终单文件可执行程序。

> 开发调试期间如需启动后直接弹出主窗口，可执行 `cargo run -- --show`。若在部分显卡驱动或高 DPI 缩放（如 125%/150%）下希望切换为像素对齐的纯软件光栅渲染以获得更锐利的暗色字体边缘，可设置环境变量 `SLINT_BACKEND=winit-software` 后启动。

## 许可说明
- 本项目应用代码版权归 **Z.W.&AI** 所有；UI 框架 Slint 提供免版税（Royalty-free）桌面应用商业使用授权（Ambassador License）及 GPLv3 开源协议，实际商业闭源分发时请依据 Slint 官方授权条款确认。
