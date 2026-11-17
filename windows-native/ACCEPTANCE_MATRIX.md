# 验收标准（AC-01 至 AC-12）核验矩阵

| 编号 | 验收项 | 实现模块与核验方法 | 对应需求 |
| --- | --- | --- | --- |
| AC-01 | 报时准确 | `scheduler.rs` 采用 `250ms` 节拍检测分钟变化与槽位防重入，误差 $< 0.25\text{s}$ | FR-01..03 |
| AC-02 | 静默生效 | `Scheduler::is_quiet_at` 覆盖同日与跨日区间（如 `23:00–07:00`），主窗明确显示“静默中” | FR-04..06 |
| AC-03 | 餐饮提醒 | 独立 `meal_enabled` 开关与 `meal_hour:meal_minute` 匹配，播放内置 `meal.wav` | FR-07..08 |
| AC-04 | 音量实时 | `SettingsWindow` 拖动 `VolumeSlider` 实时触发 `AudioEngine::set_live_volume` 生效 | FR-09..10 |
| AC-05 | 校时可用 | `ntp.rs` 通过 UDP RFC 4330 取时并调用 `SetSystemTime`（必要时触发按需 UAC 提权） | FR-11..13 |
| AC-06 | 托盘交互 | `win_sys.rs` 提供 6 项托盘右键菜单；双击唤出主窗；主窗关闭隐藏不退出；仅“退出”终止 | FR-14..17 |
| AC-07 | 配置持久化 | `config.rs` 原子写入 `config.json`，支持损坏自动备份回退；设置页拦截非法分钟与相同静默首尾 | FR-21..22 |
| AC-08 | 开机自启 | `win_sys::apply_auto_start` 写入/移除 `HKCU\...\Run\HalfHourChime` 并与托盘勾选联动 | FR-23 |
| AC-09 | 单实例 | `CreateMutexW` 互斥锁 + `PostMessageW` 唤醒已有主窗口并取消最小化，不新增图标 | FR-24 |
| AC-10 | 高 DPI | `app.manifest` 声明 `PerMonitorV2`，Slint 使用逻辑像素布局在 125%/150% 下清晰不裁切 | 5.3 |
| AC-11 | 风格一致 | `ui/theme.slint` 统一管理 3 套主题令牌，主窗/设置/关于三窗实时同步切换 | 5.4 |
| AC-12 | 部署简便 | `.cargo/config.toml` 启用 `+crt-static` 静态 CRT，音频与图标 `include_bytes!` 内嵌，单文件交付 | 1.2 / 5.3 |
