use chrono::{DateTime, Local, NaiveDate};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

static TRAY_ICON_RGBA: &[u8] = include_bytes!("../assets/tray_icon_32.rgba");

/// 单日单个日志文件安全上限：2 MiB（防止调试日志异常暴涨占用系统盘）
pub const DEFAULT_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// 按天归档默认保留天数：14 天（自动清理更早历史日志，方便按日期快速查看导出）
pub const DEFAULT_LOG_RETENTION_DAYS: usize = 14;
/// 同日超出单文件上限时的最大分片备份数
pub const DEFAULT_LOG_MAX_BACKUPS: usize = 3;

/// 日志级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
        }
    }
}

fn extract_log_prefix(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("half_hour_chime");
    if stem.len() > 11 {
        let split_at = stem.len() - 11;
        let (prefix_part, suffix_part) = stem.split_at(split_at);
        if suffix_part.starts_with('_')
            && NaiveDate::parse_from_str(&suffix_part[1..], "%Y-%m-%d").is_ok()
        {
            return prefix_part.to_string();
        }
    }
    stem.to_string()
}

fn daily_log_filename(prefix: &str, date: NaiveDate) -> String {
    format!("{prefix}_{}.log", date.format("%Y-%m-%d"))
}

fn parse_daily_archive_date(file_name: &str, prefix: &str) -> Option<NaiveDate> {
    let expected_prefix = format!("{prefix}_");
    let rest = file_name.strip_prefix(&expected_prefix)?;
    let date_str = rest.strip_suffix(".log")?;
    if date_str.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

/// 轻量级按天归档调试日志器（格式 half_hour_chime_YYYY-MM-DD.log，跨日自动切换并按保留天数清理，Windows CRLF 换行）
pub struct RotatingLogger {
    log_dir: PathBuf,
    file_prefix: String,
    current_date: NaiveDate,
    file_path: PathBuf,
    max_bytes: u64,
    retention_days: usize,
    min_level: LogLevel,
    current_size: u64,
    writer: Option<BufWriter<File>>,
}

impl RotatingLogger {
    pub fn new(
        base_file_path: PathBuf,
        max_bytes: u64,
        retention_days: usize,
        min_level: LogLevel,
    ) -> Self {
        Self::new_at_date(
            base_file_path,
            Local::now().date_naive(),
            max_bytes,
            retention_days,
            min_level,
        )
    }

    pub fn new_at_date(
        base_file_path: PathBuf,
        initial_date: NaiveDate,
        max_bytes: u64,
        retention_days: usize,
        min_level: LogLevel,
    ) -> Self {
        let log_dir = base_file_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let file_prefix = extract_log_prefix(&base_file_path);
        let file_path = log_dir.join(daily_log_filename(&file_prefix, initial_date));
        let _ = fs::create_dir_all(&log_dir);

        let mut logger = Self {
            log_dir,
            file_prefix,
            current_date: initial_date,
            file_path,
            max_bytes: max_bytes.max(128),
            retention_days: retention_days.max(1),
            min_level,
            current_size: 0,
            writer: None,
        };
        logger.open_active_file();
        logger.prune_old_daily_logs();
        logger
    }

    pub fn path(&self) -> &Path {
        &self.file_path
    }

    pub fn set_min_level(&mut self, level: LogLevel) {
        self.min_level = level;
    }

    fn open_active_file(&mut self) {
        let _ = fs::create_dir_all(&self.log_dir);
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
        {
            Ok(file) => {
                self.current_size = file.metadata().map(|m| m.len()).unwrap_or(0);
                self.writer = Some(BufWriter::new(file));
            }
            Err(_) => {
                self.writer = None;
                self.current_size = 0;
            }
        }
    }

    fn backup_path(&self, index: usize) -> PathBuf {
        let base = self.file_path.to_string_lossy();
        PathBuf::from(format!("{base}.{index}"))
    }

    fn prune_old_daily_logs(&self) {
        let Ok(read_dir) = fs::read_dir(&self.log_dir) else {
            return;
        };
        let mut dated_files: Vec<(NaiveDate, PathBuf)> = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let name_str = name.to_str()?;
                let dt = parse_daily_archive_date(name_str, &self.file_prefix)?;
                Some((dt, entry.path()))
            })
            .collect();

        dated_files.sort_by_key(|(dt, _)| *dt);
        if dated_files.len() <= self.retention_days {
            return;
        }

        let remove_count = dated_files.len() - self.retention_days;
        for (_, old_path) in dated_files.into_iter().take(remove_count) {
            let _ = fs::remove_file(&old_path);
            for idx in 1..=DEFAULT_LOG_MAX_BACKUPS {
                let part = PathBuf::from(format!("{}.{idx}", old_path.display()));
                let _ = fs::remove_file(&part);
            }
        }
    }

    fn rollover_to_date(&mut self, new_date: NaiveDate, timestamp: DateTime<Local>) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }
        self.current_date = new_date;
        self.file_path = self
            .log_dir
            .join(daily_log_filename(&self.file_prefix, new_date));
        self.current_size = 0;
        self.open_active_file();
        self.prune_old_daily_logs();

        let marker = format!(
            "[{}] [INFO ] [pid:{}] [logger] --- daily log archived/started (date={}, retention_days={}) ---\r\n",
            timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            std::process::id(),
            new_date.format("%Y-%m-%d"),
            self.retention_days
        );
        if let Some(w) = self.writer.as_mut() {
            if w.write_all(marker.as_bytes()).is_ok() {
                let _ = w.flush();
                self.current_size = self.current_size.saturating_add(marker.len() as u64);
            }
        }
    }

    fn rotate_same_day_overflow(&mut self, timestamp: DateTime<Local>) {
        if let Some(mut w) = self.writer.take() {
            let _ = w.flush();
        }

        for idx in (1..=DEFAULT_LOG_MAX_BACKUPS).rev() {
            let dst = self.backup_path(idx);
            if idx == DEFAULT_LOG_MAX_BACKUPS {
                let _ = fs::remove_file(&dst);
                continue;
            }
            let next_dst = self.backup_path(idx + 1);
            if dst.exists() {
                let _ = fs::remove_file(&next_dst);
                let _ = fs::rename(&dst, &next_dst);
            }
        }

        if self.file_path.exists() {
            let first_backup = self.backup_path(1);
            let _ = fs::remove_file(&first_backup);
            let _ = fs::rename(&self.file_path, &first_backup);
        }

        self.current_size = 0;
        self.open_active_file();

        let marker = format!(
            "[{}] [INFO ] [pid:{}] [logger] --- same-day log split (max_bytes={}, backups={}) ---\r\n",
            timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            std::process::id(),
            self.max_bytes,
            DEFAULT_LOG_MAX_BACKUPS
        );
        if let Some(w) = self.writer.as_mut() {
            if w.write_all(marker.as_bytes()).is_ok() {
                let _ = w.flush();
                self.current_size = self.current_size.saturating_add(marker.len() as u64);
            }
        }
    }

    pub fn log(&mut self, level: LogLevel, target: &str, message: &str) {
        self.log_at(Local::now(), level, target, message);
    }

    pub fn log_at(
        &mut self,
        timestamp: DateTime<Local>,
        level: LogLevel,
        target: &str,
        message: &str,
    ) {
        if level < self.min_level {
            return;
        }

        let msg_date = timestamp.date_naive();
        if msg_date != self.current_date {
            self.rollover_to_date(msg_date, timestamp);
        }

        let thread_label = std::thread::current()
            .name()
            .unwrap_or("worker")
            .to_string();
        let clean_msg = message.replace(['\r', '\n'], " ");
        let line = format!(
            "[{}] [{}] [pid:{}] [{}] [{}] {}\r\n",
            timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            level.as_str(),
            std::process::id(),
            thread_label,
            target,
            clean_msg
        );

        let incoming_len = line.len() as u64;
        if self.writer.is_none() {
            self.open_active_file();
        }
        if self.current_size > 0 && self.current_size.saturating_add(incoming_len) > self.max_bytes {
            self.rotate_same_day_overflow(timestamp);
        }

        if let Some(w) = self.writer.as_mut() {
            if w.write_all(line.as_bytes()).is_ok() {
                let _ = w.flush();
                self.current_size = self.current_size.saturating_add(incoming_len);
            }
        }
    }

    pub fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.flush();
        }
    }
}

static GLOBAL_LOGGER: OnceLock<Mutex<RotatingLogger>> = OnceLock::new();

fn is_dir_writable_for_log(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".chime_log_probe.tmp");
    match fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 按运行环境优先级解析可写日志目录：
/// 1) 传入基准目录/logs（通常与 config.json 同目录，如 dist\logs，便于按天查看导出）
/// 2) 当前 exe 同级 logs 目录
/// 3) %APPDATA%\HalfHourChime\logs
/// 4) %TEMP%\HalfHourChime\logs（只读系统目录兜底）
pub fn resolve_log_file_path(preferred_base_dir: Option<&Path>) -> PathBuf {
    let today_name = daily_log_filename("half_hour_chime", Local::now().date_naive());
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(base) = preferred_base_dir {
        candidates.push(base.join("logs"));
    }
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            candidates.push(exe_dir.join("logs"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("HalfHourChime").join("logs"));
    }
    candidates.push(std::env::temp_dir().join("HalfHourChime").join("logs"));

    for dir in candidates {
        if is_dir_writable_for_log(&dir) {
            return dir.join(today_name);
        }
    }
    std::env::temp_dir().join(today_name)
}

/// 初始化全局按天归档的调试日志器（应在主程序启动早期调用）
pub fn init_logger(preferred_base_dir: Option<&Path>) -> PathBuf {
    let min_level = if std::env::var_os("HALF_HOUR_CHIME_DEBUG").is_some()
        || cfg!(debug_assertions)
    {
        LogLevel::Debug
    } else {
        LogLevel::Info
    };

    let logger_mutex = GLOBAL_LOGGER.get_or_init(|| {
        let path = resolve_log_file_path(preferred_base_dir);
        Mutex::new(RotatingLogger::new(
            path,
            DEFAULT_LOG_MAX_BYTES,
            DEFAULT_LOG_RETENTION_DAYS,
            min_level,
        ))
    });

    if let Ok(guard) = logger_mutex.lock() {
        guard.path().to_path_buf()
    } else {
        resolve_log_file_path(preferred_base_dir)
    }
}

fn with_global_logger<F: FnOnce(&mut RotatingLogger)>(f: F) {
    let logger_mutex = GLOBAL_LOGGER.get_or_init(|| {
        let path = resolve_log_file_path(None);
        let min_level = if cfg!(debug_assertions) {
            LogLevel::Debug
        } else {
            LogLevel::Info
        };
        Mutex::new(RotatingLogger::new(
            path,
            DEFAULT_LOG_MAX_BYTES,
            DEFAULT_LOG_RETENTION_DAYS,
            min_level,
        ))
    });
    if let Ok(mut guard) = logger_mutex.lock() {
        f(&mut guard);
    }
}

pub fn current_log_path() -> Option<PathBuf> {
    GLOBAL_LOGGER
        .get()
        .and_then(|m| m.lock().ok())
        .map(|g| g.path().to_path_buf())
}

pub fn set_log_level(level: LogLevel) {
    with_global_logger(|logger| logger.set_min_level(level));
}

pub fn flush_logs() {
    with_global_logger(|logger| logger.flush());
}

pub fn log_msg(level: LogLevel, target: &str, message: &str) {
    with_global_logger(|logger| logger.log(level, target, message));
}

pub fn log_debug(target: &str, message: &str) {
    log_msg(LogLevel::Debug, target, message);
}

pub fn log_info(target: &str, message: &str) {
    log_msg(LogLevel::Info, target, message);
}

pub fn log_warn(target: &str, message: &str) {
    log_msg(LogLevel::Warn, target, message);
}

pub fn log_error(target: &str, message: &str) {
    log_msg(LogLevel::Error, target, message);
}

/// 记录当前运行环境关键快照，便于排查权限、只读目录与音频/网络异常
pub fn log_environment_snapshot(app_version: &str, config_path: &Path) {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let log_path = current_log_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "<uninitialized>".to_string());
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "<unknown>".to_string());
    let computer = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "<unknown>".to_string());

    log_info(
        "env",
        &format!(
            "startup version={app_version} os={} arch={} computer={computer} user={user}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    log_info(
        "env",
        &format!(
            "paths exe=\"{exe}\" cwd=\"{cwd}\" config=\"{}\" log=\"{log_path}\"",
            config_path.display()
        ),
    );
}

/// 托盘右键菜单命令 ID 集合（FR-15 + 快捷操作）
pub struct TrayMenuIds {
    pub test_chime: MenuId,
    pub stop_audio: MenuId,
    pub sync_ntp: MenuId,
    pub mute_mode: MenuId,
    pub open_settings: MenuId,
    pub auto_start: MenuId,
    pub open_logs_folder: MenuId,
    pub check_update: MenuId,
    pub about: MenuId,
    pub exit: MenuId,
}

pub struct TrayController {
    pub _tray_icon: TrayIcon,
    pub auto_start_item: CheckMenuItem,
    pub mute_mode_item: CheckMenuItem,
    pub ids: TrayMenuIds,
}

fn format_tray_tooltip(muted: bool) -> &'static str {
    if muted {
        "半整点提醒（静音模式） - 点击打开主窗口"
    } else {
        "半整点提醒 - 点击打开主窗口"
    }
}

impl TrayController {
    pub fn create(initial_auto_start: bool, initial_mute_mode: bool) -> Result<Self, String> {
        let icon = Icon::from_rgba(TRAY_ICON_RGBA.to_vec(), 32, 32)
            .map_err(|e| format!("加载托盘图标失败：{e}"))?;

        let menu = Menu::new();
        let item_test = MenuItem::new("测试报时", true, None);
        let item_stop = MenuItem::new("停止当前播报", true, None);
        let item_sync = MenuItem::new("立即校时", true, None);
        let item_mute = CheckMenuItem::new("静音模式", true, initial_mute_mode, None);
        let item_settings = MenuItem::new("设置与闹钟...", true, None);
        let item_autostart = CheckMenuItem::new("开机自启", true, initial_auto_start, None);
        let item_logs = MenuItem::new("打开日志文件夹", true, None);
        let item_update = MenuItem::new("检查更新", true, None);
        let item_about = MenuItem::new("关于", true, None);
        let item_exit = MenuItem::new("退出", true, None);

        let ids = TrayMenuIds {
            test_chime: item_test.id().clone(),
            stop_audio: item_stop.id().clone(),
            sync_ntp: item_sync.id().clone(),
            mute_mode: item_mute.id().clone(),
            open_settings: item_settings.id().clone(),
            auto_start: item_autostart.id().clone(),
            open_logs_folder: item_logs.id().clone(),
            check_update: item_update.id().clone(),
            about: item_about.id().clone(),
            exit: item_exit.id().clone(),
        };

        let _ = menu.append(&item_test);
        let _ = menu.append(&item_stop);
        let _ = menu.append(&item_sync);
        let _ = menu.append(&item_mute);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_settings);
        let _ = menu.append(&item_autostart);
        let _ = menu.append(&item_logs);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_update);
        let _ = menu.append(&item_about);
        let _ = menu.append(&item_exit);

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format_tray_tooltip(initial_mute_mode))
            .with_icon(icon)
            .build()
            .map_err(|e| format!("初始化系统托盘失败：{e}"))?;

        Ok(Self {
            _tray_icon: tray_icon,
            auto_start_item: item_autostart,
            mute_mode_item: item_mute,
            ids,
        })
    }

    pub fn set_auto_start_checked(&self, checked: bool) {
        self.auto_start_item.set_checked(checked);
    }

    pub fn set_mute_mode_checked(&self, checked: bool) {
        self.mute_mode_item.set_checked(checked);
        let _ = self._tray_icon.set_tooltip(Some(format_tray_tooltip(checked)));
    }
}

/// 开机自启注册表路径与键名
#[cfg(target_os = "windows")]
const RUN_KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(target_os = "windows")]
const RUN_VALUE_NAME: &str = "HalfHourChime";

/// 注册或注销当前程序到 Windows 当前用户开机启动项（FR-23 / AC-08）
pub fn apply_auto_start(enable: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
            RegOpenKeyExW, RegSetValueExW,
        };

        let subkey = HSTRING::from(RUN_KEY_PATH);
        let val_name = HSTRING::from(RUN_VALUE_NAME);

        unsafe {
            let mut hkey = HKEY::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            );
            if status.is_err() {
                return Err(format!("无法打开自启注册表项：{status:?}"));
            }

            let res = if enable {
                let exe = std::env::current_exe()
                    .map_err(|e| format!("获取可执行文件路径失败：{e}"))?;
                let quoted = format!("\"{}\"", exe.to_string_lossy());
                let wide: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = std::slice::from_raw_parts(
                    wide.as_ptr() as *const u8,
                    wide.len() * std::mem::size_of::<u16>(),
                );
                let set_status = RegSetValueExW(
                    hkey,
                    PCWSTR(val_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(bytes),
                );
                if set_status.is_err() {
                    Err(format!("写入开机自启项失败：{set_status:?}"))
                } else {
                    Ok(())
                }
            } else {
                let _ = RegDeleteValueW(hkey, PCWSTR(val_name.as_ptr()));
                Ok(())
            };

            let _ = RegCloseKey(hkey);
            if let Err(ref err) = res {
                log_error("autostart", &format!("apply_auto_start({enable}) failed: {err}"));
            } else {
                log_info("autostart", &format!("apply_auto_start({enable}) succeeded"));
            }
            res
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        log_debug("autostart", &format!("non-windows stub apply_auto_start({enable})"));
        Ok(())
    }
}

/// 查询当前注册表中是否已配置开机自启
pub fn query_auto_start_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        };

        let subkey = HSTRING::from(RUN_KEY_PATH);
        let val_name = HSTRING::from(RUN_VALUE_NAME);

        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            )
            .is_err()
            {
                return false;
            }
            let exists = RegQueryValueExW(
                hkey,
                PCWSTR(val_name.as_ptr()),
                None,
                None,
                None,
                None,
            )
            .is_ok();
            let _ = RegCloseKey(hkey);
            exists
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// 单实例互斥体与跨进程唤醒句柄（FR-24 / AC-09）
pub struct SingleInstanceGuard {
    #[cfg(target_os = "windows")]
    mutex_handle: windows::Win32::Foundation::HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            if !self.mutex_handle.is_invalid() {
                let _ = windows::Win32::Foundation::CloseHandle(self.mutex_handle);
            }
        }
    }
}

#[cfg(target_os = "windows")]
const IPC_WINDOW_CLASS: &str = "HalfHourChime_IPC_Window_v2";
#[cfg(target_os = "windows")]
const IPC_WAKE_MSG_NAME: &str = "HalfHourChime_ShowMainWindow_v2";

/// 检查是否已有实例运行：若已存在则发送消息唤醒已有实例并返回 Ok(None)；否则持有锁并返回 Ok(Some(guard))
pub fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>, String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, RegisterWindowMessageW};

        unsafe {
            let handle = CreateMutexW(None, false, w!("Local\\HalfHourChime_SingleInstance_Mutex_v2"))
                .map_err(|e| format!("创建单实例互斥锁失败：{e}"))?;

            if GetLastError() == ERROR_ALREADY_EXISTS {
                log_warn("instance", "duplicate instance detected, waking existing window");
                let cls = HSTRING::from(IPC_WINDOW_CLASS);
                let msg_name = HSTRING::from(IPC_WAKE_MSG_NAME);
                if let Ok(hwnd) = FindWindowW(PCWSTR(cls.as_ptr()), None) {
                    if !hwnd.is_invalid() {
                        let wake_msg = RegisterWindowMessageW(PCWSTR(msg_name.as_ptr()));
                        if wake_msg != 0 {
                            let _ = PostMessageW(hwnd, wake_msg, WPARAM(0), LPARAM(0));
                        }
                    }
                }
                let _ = CloseHandle(handle);
                return Ok(None);
            }

            log_debug("instance", "single-instance mutex acquired");
            Ok(Some(SingleInstanceGuard {
                mutex_handle: handle,
            }))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(Some(SingleInstanceGuard {}))
    }
}

/// 在后台轻量 Win32 消息窗口线程中监听“重复启动唤醒”消息，收到后触发回调显示主窗
pub fn start_ipc_wake_listener<F>(on_wake: F)
where
    F: Fn() + Send + Sync + 'static,
{
    #[cfg(target_os = "windows")]
    {
        use std::sync::{Arc, OnceLock};
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
            RegisterClassW, RegisterWindowMessageW, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW,
            WS_OVERLAPPED,
        };

        static WAKE_CALLBACK: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();
        static WAKE_MSG_ID: OnceLock<u32> = OnceLock::new();

        let _ = WAKE_CALLBACK.set(Arc::new(on_wake));

        unsafe extern "system" fn ipc_wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if let Some(&wake_id) = WAKE_MSG_ID.get() {
                if msg == wake_id && wake_id != 0 {
                    if let Some(cb) = WAKE_CALLBACK.get() {
                        cb();
                    }
                    return LRESULT(0);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        let _ = std::thread::Builder::new()
            .name("ipc-wake-listener".to_string())
            .spawn(move || unsafe {
                let msg_name = HSTRING::from(IPC_WAKE_MSG_NAME);
                let wake_id = RegisterWindowMessageW(PCWSTR(msg_name.as_ptr()));
                let _ = WAKE_MSG_ID.set(wake_id);

                let hinstance = GetModuleHandleW(None).unwrap_or_default();
                let cls_name = HSTRING::from(IPC_WINDOW_CLASS);

                let wc = WNDCLASSW {
                    lpfnWndProc: Some(ipc_wnd_proc),
                    hInstance: hinstance.into(),
                    lpszClassName: PCWSTR(cls_name.as_ptr()),
                    ..Default::default()
                };
                RegisterClassW(&wc);

                let _hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(cls_name.as_ptr()),
                    PCWSTR(cls_name.as_ptr()),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    hinstance,
                    None,
                );

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = on_wake;
    }
}

/// 读取 Windows 当前用户的系统 HTTP/HTTPS 代理设置（Internet Settings）
pub fn read_windows_proxy_url() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
            RegOpenKeyExW, RegQueryValueExW,
        };

        let subkey = HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
        unsafe {
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                0,
                KEY_READ,
                &mut hkey,
            )
            .is_err()
            {
                return None;
            }

            let mut enabled_val: u32 = 0;
            let mut enabled_size = std::mem::size_of::<u32>() as u32;
            let mut val_type = REG_VALUE_TYPE::default();
            let enable_name = HSTRING::from("ProxyEnable");
            let enable_ok = RegQueryValueExW(
                hkey,
                PCWSTR(enable_name.as_ptr()),
                None,
                Some(&mut val_type),
                Some(&mut enabled_val as *mut u32 as *mut u8),
                Some(&mut enabled_size),
            )
            .is_ok()
                && val_type == REG_DWORD
                && enabled_val != 0;

            if !enable_ok {
                let _ = RegCloseKey(hkey);
                return None;
            }

            let server_name = HSTRING::from("ProxyServer");
            let mut buf = vec![0u16; 512];
            let mut buf_bytes = (buf.len() * std::mem::size_of::<u16>()) as u32;
            let mut str_type = REG_VALUE_TYPE::default();
            let server_ok = RegQueryValueExW(
                hkey,
                PCWSTR(server_name.as_ptr()),
                None,
                Some(&mut str_type),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut buf_bytes),
            )
            .is_ok()
                && str_type == REG_SZ;
            let _ = RegCloseKey(hkey);

            if !server_ok {
                return None;
            }

            let u16_len = (buf_bytes as usize / 2).min(buf.len());
            let slice = &buf[..u16_len];
            let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            let raw = String::from_utf16_lossy(&slice[..end]).trim().to_string();
            if raw.is_empty() {
                return None;
            }

            // 兼容 "127.0.0.1:7890" 与 "http=127.0.0.1:7890;https=127.0.0.1:7890" 格式
            let picked = if raw.contains('=') {
                raw.split(';')
                    .filter_map(|part| {
                        let p = part.trim();
                        p.strip_prefix("https=")
                            .or_else(|| p.strip_prefix("http="))
                            .map(|s| s.trim().to_string())
                    })
                    .next()
                    .unwrap_or(raw)
            } else {
                raw
            };

            if picked.starts_with("http://") || picked.starts_with("https://") {
                Some(picked)
            } else {
                Some(format!("http://{picked}"))
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 仅允许 http/https 协议，在默认浏览器中安全打开更新下载链接
pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err("仅支持打开 http:// 或 https:// 下载链接".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let url_h = HSTRING::from(trimmed);
        let res = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(url_h.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        if (res.0 as usize) <= 32 {
            return Err("无法拉起默认浏览器打开下载链接".to_string());
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}

/// 快速在系统文件管理器中打开当前按天归档日志所在目录
pub fn open_logs_folder() -> Result<(), String> {
    let log_dir = current_log_path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| {
            resolve_log_file_path(None)
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| std::env::temp_dir().join("HalfHourChime").join("logs"))
        });

    if let Err(err) = fs::create_dir_all(&log_dir) {
        log_warn(
            "tray",
            &format!("create_dir_all for logs folder \"{}\" failed: {err}", log_dir.display()),
        );
    }

    log_info(
        "tray",
        &format!("open_logs_folder dir=\"{}\"", log_dir.display()),
    );

    #[cfg(target_os = "windows")]
    {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let dir_h = HSTRING::from(log_dir.to_string_lossy().as_ref());
        let res = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(dir_h.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            )
        };
        if (res.0 as usize) <= 32 {
            let err = format!("无法打开日志文件夹：{}", log_dir.display());
            log_warn("tray", &err);
            return Err(err);
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}

/// 原地升级重启执行计划（完全免除 cmd.exe / ping.exe，不弹出任何控制台窗口）
#[derive(Debug, Clone)]
pub enum UpdateLaunchPlan {
    /// 可写目录已完成原地替换：主进程释放互斥锁与端口后直接拉起新版本 exe
    DirectRelaunch(std::path::PathBuf),
    /// 已通过 UAC 拉起 GUI 子系统提权副本：主进程只需立即释放锁并退出
    ElevatedAlreadySpawned,
    /// 安装包模式（.msi / setup.exe）：主进程释放锁后直接打开安装程序
    RunInstaller(std::path::PathBuf),
}

fn build_old_backup_path(exe_path: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.old", exe_path.to_string_lossy()))
}

fn is_dir_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".chime_update_probe_{}.tmp", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 准备自动更新替换与重启计划：
/// - 便携单文件且目录可写：直接利用 Windows 允许重命名运行中 exe 的特性，先改名为 `.old` 再跨盘复制新版本（零外部进程/零控制台窗）
/// - 受保护目录（如 Program Files）：通过 UAC 提权启动刚下载好的 GUI 子系统自身副本（`--apply-elevated-update`），全程无 cmd/ping 弹窗
pub fn prepare_update_and_build_launch(
    temp_package_path: &std::path::Path,
    is_installer_mode: bool,
) -> Result<UpdateLaunchPlan, String> {
    if is_installer_mode {
        log_info(
            "updater",
            &format!("prepared installer launch path=\"{}\"", temp_package_path.display()),
        );
        return Ok(UpdateLaunchPlan::RunInstaller(temp_package_path.to_path_buf()));
    }

    #[cfg(target_os = "windows")]
    {
        let current_exe =
            std::env::current_exe().map_err(|e| format!("获取当前程序路径失败：{e}"))?;
        let writable = current_exe
            .parent()
            .map(is_dir_writable)
            .unwrap_or(false);

        if writable {
            let old_backup = build_old_backup_path(&current_exe);
            let _ = std::fs::remove_file(&old_backup);

            match std::fs::rename(&current_exe, &old_backup) {
                Ok(()) => {
                    let copy_res = std::fs::copy(temp_package_path, &current_exe).and_then(|copied| {
                        if copied < 64 * 1024 {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "copied exe size too small",
                            ))
                        } else {
                            Ok(())
                        }
                    });
                    match copy_res {
                        Ok(()) => {
                            let _ = std::fs::remove_file(temp_package_path);
                            log_info(
                                "updater",
                                &format!(
                                    "in-place exe replaced silently dst=\"{}\" backup=\"{}\"",
                                    current_exe.display(),
                                    old_backup.display()
                                ),
                            );
                            return Ok(UpdateLaunchPlan::DirectRelaunch(current_exe));
                        }
                        Err(copy_err) => {
                            let _ = std::fs::rename(&old_backup, &current_exe);
                            log_warn("updater", &format!("in-place copy failed, rolled back: {copy_err}"));
                            return Err(format!("写入新版本程序失败：{copy_err}"));
                        }
                    }
                }
                Err(rename_err) => {
                    log_warn(
                        "updater",
                        &format!("in-place rename failed ({rename_err}), falling back to elevated self-apply"),
                    );
                }
            }
        }

        // 受保护目录或重命名受阻：用下载好的 GUI 子系统 exe 自身请求 UAC 提权完成替换（无控制台窗）
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let src_str = temp_package_path.to_string_lossy().replace('"', "");
        let dst_str = current_exe.to_string_lossy().replace('"', "");
        let exe_h = HSTRING::from(src_str.as_str());
        let params = HSTRING::from(format!(
            "--apply-elevated-update \"{src_str}\" \"{dst_str}\""
        ));

        let res = unsafe {
            ShellExecuteW(
                None,
                w!("runas"),
                PCWSTR(exe_h.as_ptr()),
                PCWSTR(params.as_ptr()),
                None,
                SW_SHOWNORMAL,
            )
        };
        if (res.0 as usize) <= 32 {
            log_warn("updater", "elevated GUI self-updater canceled by UAC");
            return Err("管理员授权被取消，无法替换受保护目录中的程序文件".to_string());
        }

        log_info(
            "updater",
            &format!(
                "spawned elevated GUI self-updater src=\"{}\" dst=\"{}\"",
                temp_package_path.display(),
                current_exe.display()
            ),
        );
        Ok(UpdateLaunchPlan::ElevatedAlreadySpawned)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = temp_package_path;
        Ok(UpdateLaunchPlan::ElevatedAlreadySpawned)
    }
}

/// 在主线程释放单实例互斥锁与本地端口之后执行最终启动动作
pub fn execute_update_launch_plan(plan: &UpdateLaunchPlan) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        match plan {
            UpdateLaunchPlan::DirectRelaunch(exe_path) => {
                std::process::Command::new(exe_path)
                    .arg("--show")
                    .spawn()
                    .map_err(|e| format!("启动新版本程序失败：{e}"))?;
            }
            UpdateLaunchPlan::RunInstaller(installer_path) => {
                use windows::core::{w, HSTRING, PCWSTR};
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

                let path_h = HSTRING::from(installer_path.to_string_lossy().as_ref());
                let res = unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        PCWSTR(path_h.as_ptr()),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    )
                };
                if (res.0 as usize) <= 32 {
                    return Err("无法启动安装程序".to_string());
                }
            }
            UpdateLaunchPlan::ElevatedAlreadySpawned => {}
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = plan;
    }
    Ok(())
}

/// 提权自更新 CLI 模式（GUI 子系统运行，零控制台窗口）：等待旧进程退出 -> 原地替换 -> 拉起新版并清理临时文件
pub fn run_elevated_update_apply_mode(src_path: &std::path::Path, dst_path: &std::path::Path) -> i32 {
    #[cfg(target_os = "windows")]
    {
        use std::time::Duration;

        std::thread::sleep(Duration::from_millis(350));
        let old_backup = build_old_backup_path(dst_path);

        for _ in 0..24 {
            let _ = std::fs::remove_file(&old_backup);
            if dst_path.exists() {
                let _ = std::fs::rename(dst_path, &old_backup);
            }
            if let Ok(copied) = std::fs::copy(src_path, dst_path) {
                if copied >= 64 * 1024 {
                    let _ = std::fs::remove_file(&old_backup);
                    let src_clean = src_path.to_string_lossy().to_string();
                    let _ = std::process::Command::new(dst_path)
                        .args(["--show", "--cleanup-temp", &src_clean])
                        .spawn();
                    return 0;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        1
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (src_path, dst_path);
        0
    }
}

/// 启动后静默清理上次原地升级遗留的 `.old` 备份文件及临时安装包（无任何控制台窗口）
pub fn spawn_cleanup_stale_update_files(cleanup_temp_arg: Option<String>) {
    let current_exe = std::env::current_exe().ok();
    let _ = std::thread::Builder::new()
        .name("update-cleanup-worker".to_string())
        .spawn(move || {
            for attempt in 0..12 {
                let mut all_done = true;

                if let Some(ref exe) = current_exe {
                    let old_backup = build_old_backup_path(exe);
                    if old_backup.exists() && std::fs::remove_file(&old_backup).is_err() {
                        all_done = false;
                    }
                }

                if let Some(ref temp_str) = cleanup_temp_arg {
                    let p = std::path::Path::new(temp_str);
                    if p.exists() && std::fs::remove_file(p).is_err() {
                        all_done = false;
                    }
                }

                if attempt == 0 {
                    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if (name.starts_with("HalfHourChime_update_")
                                || name.starts_with("half_hour_chime_updater_"))
                                && entry
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .ok()
                                    .and_then(|t| t.elapsed().ok())
                                    .map(|age| age > std::time::Duration::from_secs(3600))
                                    .unwrap_or(false)
                            {
                                let _ = std::fs::remove_file(entry.path());
                            }
                        }
                    }
                }

                if all_done {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
}

/// 呼出 Windows 原生文件选择器，允许用户挑选自定义音频文件（支持 WAV/MP3/OGG/FLAC）
pub fn browse_audio_file() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
            OPENFILENAMEW,
        };

        let mut file_buf = vec![0u16; 1024];
        let title: Vec<u16> = "选择自定义音频文件\0".encode_utf16().collect();
        let filter: Vec<u16> =
            "音频文件 (*.wav;*.mp3;*.ogg;*.flac)\0*.wav;*.mp3;*.ogg;*.flac\0所有文件 (*.*)\0*.*\0\0"
                .encode_utf16()
                .collect();

        let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = HWND::default();
        ofn.lpstrFilter = PCWSTR(filter.as_ptr());
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = PWSTR(file_buf.as_mut_ptr());
        ofn.nMaxFile = file_buf.len() as u32;
        ofn.lpstrTitle = PCWSTR(title.as_ptr());
        ofn.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;

        let selected = unsafe { GetOpenFileNameW(&mut ofn).as_bool() };
        if selected {
            let len = file_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(file_buf.len());
            let path = String::from_utf16_lossy(&file_buf[..len]).trim().to_string();
            if !path.is_empty() {
                log_info("audio", &format!("selected custom audio: \"{path}\""));
                return Some(path);
            }
        }
        None
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 获取主显示器可用工作区坐标（扣除任务栏区域），返回 (left, top, right, bottom)
pub fn get_primary_work_area() -> Option<(i32, i32, i32, i32)> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };

        let mut rect = RECT::default();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut rect as *mut RECT as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_ok()
        };
        if ok && rect.right > rect.left && rect.bottom > rect.top {
            return Some((rect.left, rect.top, rect.right, rect.bottom));
        }
        None
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 为无边框窗体应用硬件圆角（Win11 使用 DWMWA_WINDOW_CORNER_PREFERENCE，Win10 自动回退为 GDI 圆角区域）
pub fn apply_rounded_window_corners(
    hwnd_isize: isize,
    width_px: i32,
    height_px: i32,
    radius_px: i32,
) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWCP_ROUND, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWM_WINDOW_CORNER_PREFERENCE,
        };
        use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};

        if hwnd_isize == 0 {
            return;
        }
        let hwnd = HWND(hwnd_isize as *mut std::ffi::c_void);
        unsafe {
            let preference: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
            let dwm_res = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as *const _,
                std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            );
            if dwm_res.is_ok() {
                // 由 Slint WindowStyle 渲染主题匹配的 1px 圆角描边，隐藏 DWM 默认系统描边避免双边框叠色
                let border_none: u32 = DWMWA_COLOR_NONE;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_BORDER_COLOR,
                    &border_none as *const _ as *const _,
                    std::mem::size_of::<u32>() as u32,
                );
            } else if width_px > 0 && height_px > 0 {
                // Windows 10 不支持 DWMWA_WINDOW_CORNER_PREFERENCE，此时回退为系统圆角裁剪区域
                let diameter = (radius_px.max(6) * 2).max(12);
                let rgn = CreateRoundRectRgn(0, 0, width_px + 1, height_px + 1, diameter, diameter);
                if !rgn.is_invalid() {
                    let _ = SetWindowRgn(hwnd, rgn, true);
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd_isize, width_px, height_px, radius_px);
    }
}

/// 将指定窗体置顶显示（用于闹钟响铃提醒弹窗）
pub fn bring_window_topmost(hwnd_isize: isize) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        };

        if hwnd_isize == 0 {
            return;
        }
        let hwnd = HWND(hwnd_isize as *mut std::ffi::c_void);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd_isize;
    }
}

/// 将指定窗体拉起到前台并激活（不锁定常驻置顶，用于托盘左键点击/单实例唤醒）
pub fn bring_window_foreground(hwnd_isize: isize) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            IsIconic, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST,
            SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
        };

        if hwnd_isize == 0 {
            return;
        }
        let hwnd = HWND(hwnd_isize as *mut std::ffi::c_void);
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = SetWindowPos(
                hwnd,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = SetForegroundWindow(hwnd);
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd_isize;
    }
}

/// 调用 Windows 原生 SAPI 5（ISpVoice）执行文字转语音播报（支持 35ms 级取消打断，零外部依赖）
pub fn speak_text_sapi_cancellable<F>(
    text: &str,
    volume: u8,
    rate: i8,
    mut should_cancel: F,
) -> Result<bool, String>
where
    F: FnMut() -> bool,
{
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(true);
    }
    if should_cancel() {
        return Ok(false);
    }

    #[cfg(target_os = "windows")]
    {
        use std::time::{Duration, Instant};
        use windows::core::PCWSTR;
        use windows::Win32::Media::Speech::{ISpVoice, SpVoice, SPRS_DONE, SPVOICESTATUS};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL,
            COINIT_APARTMENTTHREADED,
        };

        // SPF_ASYNC (1) | SPF_PURGEBEFORESPEAK (2) = 3：异步启动并立即清除自身缓冲
        const SPF_ASYNC_PURGE: u32 = 1 | 2;

        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let need_uninit = hr.is_ok();

            let res = (|| -> Result<bool, String> {
                let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)
                    .map_err(|e| format!("初始化 Windows 语音引擎失败：{e}"))?;
                let _ = voice.SetVolume(volume.min(100) as u16);
                let _ = voice.SetRate(rate.clamp(-10, 10) as i32);

                let wide: Vec<u16> = trimmed.encode_utf16().chain(std::iter::once(0)).collect();
                voice
                    .Speak(PCWSTR(wide.as_ptr()), SPF_ASYNC_PURGE, None)
                    .map_err(|e| format!("Windows 语音播报失败：{e}"))?;

                let started = Instant::now();
                let max_wait = Duration::from_secs(60);
                let mut completed = true;

                while started.elapsed() < max_wait {
                    if should_cancel() {
                        let _ = voice.Speak(PCWSTR::null(), SPF_ASYNC_PURGE, None);
                        let _ = voice.WaitUntilDone(80);
                        completed = false;
                        break;
                    }
                    let _ = voice.WaitUntilDone(35);
                    let mut status = SPVOICESTATUS::default();
                    if voice
                        .GetStatus(&mut status, std::ptr::null_mut())
                        .is_err()
                        || status.dwRunningState == SPRS_DONE.0 as u32
                    {
                        break;
                    }
                }
                Ok(completed)
            })();

            if need_uninit {
                CoUninitialize();
            }
            res
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (trimmed, volume, rate, &mut should_cancel);
        Ok(true)
    }
}

/// 调用 Windows 原生 SAPI 5（ISpVoice）执行文字转语音同步播报（零外部依赖）
#[allow(dead_code)]
pub fn speak_text_sapi(text: &str, volume: u8, rate: i8) -> Result<(), String> {
    speak_text_sapi_cancellable(text, volume, rate, || false).map(|_| ())
}

/// 查询当前进程实时工作集（WorkingSetSize）与私有提交内存（PrivateUsage），返回单位为字节
pub fn query_process_memory_bytes() -> (u64, u64) {
    #[cfg(target_os = "windows")]
    {
        use std::mem;
        use windows::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows::Win32::System::Threading::GetCurrentProcess;

        unsafe {
            let mut pmc: PROCESS_MEMORY_COUNTERS_EX = mem::zeroed();
            pmc.cb = mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX as *mut _,
                pmc.cb,
            )
            .is_ok()
            {
                (pmc.WorkingSetSize as u64, pmc.PrivateUsage as u64)
            } else {
                (0, 0)
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        (0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn daily_logger_archives_by_date_and_enforces_retention_days() {
        let unique = format!(
            "chime_daily_log_{}_{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(unique);
        let base_path = dir.join("half_hour_chime.log");

        let d1 = Local.with_ymd_and_hms(2026, 11, 12, 23, 55, 0).unwrap();
        let d2 = Local.with_ymd_and_hms(2026, 11, 13, 0, 10, 0).unwrap();
        let d3 = Local.with_ymd_and_hms(2026, 11, 14, 9, 30, 0).unwrap();

        // 保留最近 2 天（d2、d3），验证跨日自动归档且 d1 被按保留天数清理
        let mut logger = RotatingLogger::new_at_date(
            base_path,
            d1.date_naive(),
            64 * 1024,
            2,
            LogLevel::Debug,
        );
        logger.log_at(d1, LogLevel::Info, "app", "day1 event");
        logger.log_at(d2, LogLevel::Info, "app", "day2 event");
        logger.log_at(d3, LogLevel::Info, "app", "day3 event");
        logger.flush();

        let f1 = dir.join("half_hour_chime_2026-11-12.log");
        let f2 = dir.join("half_hour_chime_2026-11-13.log");
        let f3 = dir.join("half_hour_chime_2026-11-14.log");

        assert!(!f1.exists(), "oldest daily archive beyond retention_days=2 should be pruned");
        assert!(f2.exists(), "2026-11-13 daily archive should exist");
        assert!(f3.exists(), "2026-11-14 active daily log should exist");

        let c2 = fs::read_to_string(&f2).expect("read day2 log");
        let c3 = fs::read_to_string(&f3).expect("read day3 log");
        assert!(c2.contains("day2 event"));
        assert!(c3.contains("day3 event"));
        assert!(c3.contains("\r\n"), "must use CRLF for Windows Notepad");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn daily_logger_splits_same_day_overflow_and_filters_levels() {
        let unique = format!(
            "chime_log_split_{}_{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(unique);
        let log_path = dir.join("level.log");

        let day = Local.with_ymd_and_hms(2026, 11, 14, 14, 0, 0).unwrap();
        let mut logger =
            RotatingLogger::new_at_date(log_path, day.date_naive(), 200, 7, LogLevel::Warn);
        logger.log_at(day, LogLevel::Debug, "sched", "should be filtered debug");
        logger.log_at(day, LogLevel::Info, "sched", "should be filtered info");
        for i in 0..6 {
            logger.log_at(
                day,
                LogLevel::Warn,
                "ntp",
                &format!("overflow guard #{i:02}\nsecond line"),
            );
        }
        logger.flush();

        let active = dir.join("level_2026-11-14.log");
        let split_1 = dir.join("level_2026-11-14.log.1");
        assert!(active.exists());
        assert!(split_1.exists());

        let content = fs::read_to_string(&active).expect("read active daily log");
        assert!(!content.contains("should be filtered"));
        assert!(content.contains("[WARN ]"));
        assert!(content.contains("overflow guard #05 second line"));

        let _ = fs::remove_dir_all(&dir);
    }
}

