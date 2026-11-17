#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod ntp;
mod scheduler;
mod version;
mod web_console;
mod win_sys;

use audio::AudioEngine;
use chrono::{Datelike, Local, Timelike, Weekday};
use config::{
    edge_voice_from_index, edge_voice_to_index, render_holiday_template, render_tts_template,
    AppConfig, ConfigStore, ThemeMode, TtsEngine,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use scheduler::{
    fetch_holiday_feed_from_source, solar_to_lunar, ExternalHolidayEntry, HolidayInfo, HolidayKind,
    Scheduler, TriggerEvent,
};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, TrayIconEvent};
use web_console::{
    WebAction, WebConfigPatch, WebConfigView, WebConsoleController, WebSnapshot,
    DEFAULT_WEB_CONSOLE_PORT, DEFAULT_WEB_CONSOLE_URL,
};
use win_sys::TrayController;

slint::include_modules!();

struct AppState {
    config_store: ConfigStore,
    config: AppConfig,
    live_volume: u8,
    scheduler: Scheduler,
    is_syncing: bool,
    ntp_status_code: i32, // 0: 未校时 | 1: 校时中 | 2: 成功 | 3: 失败
    ntp_status_text: String,
    holiday_feed_status: String,
    tts_preview_status: String,
}

/// 后台定期版本检查消息指令
enum UpdateWorkerMsg {
    CheckNow {
        url: String,
        token: String,
        manual: bool,
    },
    Reconfigure {
        enabled: bool,
        url: String,
        token: String,
        interval_hours: u16,
    },
    Shutdown,
}

#[derive(Clone, Default)]
struct UpdateContext {
    download_url: Arc<Mutex<String>>,
    api_download_url: Arc<Mutex<String>>,
    github_token: Arc<Mutex<String>>,
    package_type: Arc<Mutex<String>>,
    last_prompted_version: Arc<Mutex<String>>,
    is_checking: Arc<AtomicBool>,
    has_update: Arc<AtomicBool>,
    remote_version: Arc<Mutex<String>>,
    status_text: Arc<Mutex<String>>,
    release_notes: Arc<Mutex<String>>,
    is_downloading: Arc<AtomicBool>,
    download_progress: Arc<AtomicI32>,
    download_status_text: Arc<Mutex<String>>,
}

/// Windows 原生性能计数器监控与内存强制回收模块（保障网吧低配长期值守稳定运行）
mod perf_guard {
    use super::audio::AudioEngine;
    use super::win_sys;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// 内存常驻告警与强制回收阈值：50 MiB
    pub const MEMORY_GC_THRESHOLD_BYTES: u64 = 50 * 1024 * 1024;
    /// 本进程 CPU 负载偏高阈值（%）
    pub const PROCESS_CPU_HIGH_THRESHOLD_PCT: f64 = 15.0;
    /// 网吧整机 CPU 负载偏高阈值（%，触发后台非核心任务让步）
    pub const SYSTEM_CPU_HIGH_THRESHOLD_PCT: f64 = 85.0;
    /// 后台巡检采样间隔（30 秒一次，自身开销几乎为 0）
    pub const PERF_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
    /// 强制回收冷却间隔，避免高压环境下反复抖动工作集
    pub const GC_COOLDOWN: Duration = Duration::from_secs(120);

    #[derive(Debug, Clone, Copy, PartialEq, Default)]
    pub struct PerfSample {
        pub working_set_bytes: u64,
        pub private_bytes: u64,
        pub process_cpu_pct: f64,
        pub system_cpu_pct: f64,
        pub source: &'static str,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GcDecision {
        None,
        MemoryExceeded,
        CooldownActive,
    }

    pub fn format_mb(bytes: u64) -> f64 {
        (bytes as f64) / (1024.0 * 1024.0)
    }

    pub fn evaluate_gc_decision(
        sample: &PerfSample,
        elapsed_since_last_gc: Option<Duration>,
        threshold_bytes: u64,
        cooldown: Duration,
    ) -> GcDecision {
        let exceeded =
            sample.working_set_bytes > threshold_bytes || sample.private_bytes > threshold_bytes;
        if !exceeded {
            return GcDecision::None;
        }
        if let Some(elapsed) = elapsed_since_last_gc {
            if elapsed < cooldown {
                return GcDecision::CooldownActive;
            }
        }
        GcDecision::MemoryExceeded
    }

    pub fn is_cpu_overloaded(sample: &PerfSample) -> bool {
        sample.process_cpu_pct >= PROCESS_CPU_HIGH_THRESHOLD_PCT
            || sample.system_cpu_pct >= SYSTEM_CPU_HIGH_THRESHOLD_PCT
    }

    #[cfg(target_os = "windows")]
    mod imp {
        use super::*;
        use std::mem;
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};
        use windows::Win32::System::Performance::{
            PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
            PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
        };
        use windows::Win32::System::ProcessStatus::{
            EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetProcessTimes, GetSystemTimes,
        };

        fn filetime_to_u64(ft: FILETIME) -> u64 {
            u64::from(ft.dwLowDateTime) | (u64::from(ft.dwHighDateTime) << 32)
        }

        fn to_wide_null(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        /// Windows PDH（Performance Data Helper）性能计数器查询会话
        struct PdhSession {
            query: isize,
            sys_cpu_counter: isize,
            proc_cpu_counter: isize,
            proc_private_ws_counter: isize,
            logical_cpus: f64,
        }

        impl PdhSession {
            fn open(logical_cpus: f64) -> Option<Self> {
                unsafe {
                    let mut query: isize = 0;
                    if PdhOpenQueryW(None, 0, &mut query) != 0 {
                        return None;
                    }

                    let exe_stem = std::env::current_exe()
                        .ok()
                        .and_then(|p| {
                            p.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "HalfHourChime".to_string());

                    let mut sys_cpu_counter: isize = 0;
                    let status_sys = PdhAddEnglishCounterW(
                        query,
                        w!(r"\Processor(_Total)\% Processor Time"),
                        0,
                        &mut sys_cpu_counter,
                    );
                    if status_sys != 0 {
                        let _ = PdhCloseQuery(query);
                        return None;
                    }

                    let proc_cpu_path = to_wide_null(&format!(
                        r"\Process({exe_stem})\% Processor Time"
                    ));
                    let mut proc_cpu_counter: isize = 0;
                    let status_proc_cpu = PdhAddEnglishCounterW(
                        query,
                        PCWSTR(proc_cpu_path.as_ptr()),
                        0,
                        &mut proc_cpu_counter,
                    );

                    let proc_ws_path = to_wide_null(&format!(
                        r"\Process({exe_stem})\Working Set - Private"
                    ));
                    let mut proc_private_ws_counter: isize = 0;
                    let status_proc_ws = PdhAddEnglishCounterW(
                        query,
                        PCWSTR(proc_ws_path.as_ptr()),
                        0,
                        &mut proc_private_ws_counter,
                    );

                    if status_proc_cpu != 0 || status_proc_ws != 0 {
                        let _ = PdhCloseQuery(query);
                        return None;
                    }

                    // 首次采样用于建立 PDH 速率型计数器的差值基线
                    let _ = PdhCollectQueryData(query);

                    Some(Self {
                        query,
                        sys_cpu_counter,
                        proc_cpu_counter,
                        proc_private_ws_counter,
                        logical_cpus: logical_cpus.max(1.0),
                    })
                }
            }

            fn collect(&self) -> Option<(f64, f64, u64)> {
                unsafe {
                    if PdhCollectQueryData(self.query) != 0 {
                        return None;
                    }

                    let mut sys_val: PDH_FMT_COUNTERVALUE = mem::zeroed();
                    let mut proc_val: PDH_FMT_COUNTERVALUE = mem::zeroed();
                    let mut ws_val: PDH_FMT_COUNTERVALUE = mem::zeroed();

                    let s1 = PdhGetFormattedCounterValue(
                        self.sys_cpu_counter,
                        PDH_FMT_DOUBLE,
                        None,
                        &mut sys_val,
                    );
                    let s2 = PdhGetFormattedCounterValue(
                        self.proc_cpu_counter,
                        PDH_FMT_DOUBLE,
                        None,
                        &mut proc_val,
                    );
                    let s3 = PdhGetFormattedCounterValue(
                        self.proc_private_ws_counter,
                        PDH_FMT_DOUBLE,
                        None,
                        &mut ws_val,
                    );

                    if s1 != 0 || s2 != 0 {
                        return None;
                    }

                    let sys_pct = sys_val.Anonymous.doubleValue.clamp(0.0, 100.0);
                    // PDH \Process(*)\% Processor Time 最高可达 100 * 逻辑核数，这里归一化为整机百分比
                    let proc_pct =
                        (proc_val.Anonymous.doubleValue / self.logical_cpus).clamp(0.0, 100.0);
                    let private_ws = if s3 == 0 {
                        ws_val.Anonymous.doubleValue.max(0.0) as u64
                    } else {
                        0
                    };
                    Some((sys_pct, proc_pct, private_ws))
                }
            }
        }

        impl Drop for PdhSession {
            fn drop(&mut self) {
                unsafe {
                    let _ = PdhCloseQuery(self.query);
                }
            }
        }

        /// 内核态时间片兜底追踪器（兼容精简版网吧系统禁用 PDH 性能注册表的情况）
        struct KernelCpuTracker {
            prev_proc_ticks: u64,
            prev_sys_idle: u64,
            prev_sys_total: u64,
            logical_cpus: f64,
        }

        impl KernelCpuTracker {
            fn new() -> Self {
                let mut sys_info = SYSTEM_INFO::default();
                unsafe {
                    GetSystemInfo(&mut sys_info);
                }
                let logical_cpus = f64::from(sys_info.dwNumberOfProcessors.max(1));
                let mut tracker = Self {
                    prev_proc_ticks: 0,
                    prev_sys_idle: 0,
                    prev_sys_total: 0,
                    logical_cpus,
                };
                let _ = tracker.sample_cpu_pct();
                tracker
            }

            fn sample_cpu_pct(&mut self) -> (f64, f64) {
                unsafe {
                    let mut idle_ft = FILETIME::default();
                    let mut kernel_ft = FILETIME::default();
                    let mut user_ft = FILETIME::default();
                    let sys_ok = GetSystemTimes(
                        Some(&mut idle_ft),
                        Some(&mut kernel_ft),
                        Some(&mut user_ft),
                    )
                    .is_ok();

                    let mut create_ft = FILETIME::default();
                    let mut exit_ft = FILETIME::default();
                    let mut proc_kernel_ft = FILETIME::default();
                    let mut proc_user_ft = FILETIME::default();
                    let proc_ok = GetProcessTimes(
                        GetCurrentProcess(),
                        &mut create_ft,
                        &mut exit_ft,
                        &mut proc_kernel_ft,
                        &mut proc_user_ft,
                    )
                    .is_ok();

                    if !sys_ok || !proc_ok {
                        return (0.0, 0.0);
                    }

                    let idle = filetime_to_u64(idle_ft);
                    let sys_total =
                        filetime_to_u64(kernel_ft).saturating_add(filetime_to_u64(user_ft));
                    let proc_ticks = filetime_to_u64(proc_kernel_ft)
                        .saturating_add(filetime_to_u64(proc_user_ft));

                    let delta_total = sys_total.saturating_sub(self.prev_sys_total);
                    let delta_idle = idle.saturating_sub(self.prev_sys_idle);
                    let delta_proc = proc_ticks.saturating_sub(self.prev_proc_ticks);

                    self.prev_sys_idle = idle;
                    self.prev_sys_total = sys_total;
                    self.prev_proc_ticks = proc_ticks;

                    if delta_total == 0 {
                        return (0.0, 0.0);
                    }

                    let sys_busy = delta_total.saturating_sub(delta_idle);
                    let sys_pct = ((sys_busy as f64 / delta_total as f64) * 100.0).clamp(0.0, 100.0);
                    // GetSystemTimes 的总核时已经乘过逻辑核数，这里与单进程核时一致，直接比值即整机占比
                    let proc_pct =
                        ((delta_proc as f64 / delta_total as f64) * 100.0).clamp(0.0, 100.0);
                    let _ = self.logical_cpus;
                    (sys_pct, proc_pct)
                }
            }
        }

        pub fn query_process_memory_bytes() -> (u64, u64) {
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

        /// 执行 Windows 原生强制内存回收：清理空闲音频缓存 + 刷新日志缓冲 + 紧缩默认堆 + 裁剪进程工作集
        pub fn force_windows_memory_reclaim(audio: &AudioEngine) -> (u64, u64, usize) {
            let (before_ws, _) = query_process_memory_bytes();
            audio.trim_idle();
            win_sys::flush_logs();

            let mut compacted_bytes = 0usize;
            unsafe {
                if let Ok(heap) = GetProcessHeap() {
                    compacted_bytes = HeapCompact(heap, HEAP_FLAGS(0));
                }
                let _ = EmptyWorkingSet(GetCurrentProcess());
            }

            let (after_ws, _) = query_process_memory_bytes();
            (before_ws, after_ws, compacted_bytes)
        }

        pub struct NativeCollector {
            kernel_tracker: KernelCpuTracker,
            pdh_session: Option<PdhSession>,
        }

        impl NativeCollector {
            pub fn new() -> Self {
                let kernel_tracker = KernelCpuTracker::new();
                let pdh_session = PdhSession::open(kernel_tracker.logical_cpus);
                if pdh_session.is_some() {
                    win_sys::log_info(
                        "perf",
                        "initialized Windows PDH performance counters + PSAPI fallback",
                    );
                } else {
                    win_sys::log_warn(
                        "perf",
                        "PDH performance counters unavailable, using Win32 GetProcessTimes/PSAPI fallback",
                    );
                }
                Self {
                    kernel_tracker,
                    pdh_session,
                }
            }

            pub fn sample(&mut self) -> PerfSample {
                let (ws_bytes, private_bytes) = query_process_memory_bytes();
                let (k_sys_cpu, k_proc_cpu) = self.kernel_tracker.sample_cpu_pct();

                if let Some((pdh_sys, pdh_proc, pdh_ws)) =
                    self.pdh_session.as_ref().and_then(|s| s.collect())
                {
                    let effective_ws = if ws_bytes > 0 { ws_bytes } else { pdh_ws };
                    PerfSample {
                        working_set_bytes: effective_ws,
                        private_bytes: private_bytes.max(pdh_ws),
                        process_cpu_pct: pdh_proc.max(0.0),
                        system_cpu_pct: pdh_sys.max(0.0),
                        source: "pdh+psapi",
                    }
                } else {
                    PerfSample {
                        working_set_bytes: ws_bytes,
                        private_bytes,
                        process_cpu_pct: k_proc_cpu,
                        system_cpu_pct: k_sys_cpu,
                        source: "kernel-psapi",
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    mod imp {
        use super::*;

        pub struct NativeCollector;

        impl NativeCollector {
            pub fn new() -> Self {
                Self
            }

            pub fn sample(&mut self) -> PerfSample {
                PerfSample {
                    working_set_bytes: 0,
                    private_bytes: 0,
                    process_cpu_pct: 0.0,
                    system_cpu_pct: 0.0,
                    source: "non-windows-stub",
                }
            }
        }

        pub fn force_windows_memory_reclaim(audio: &AudioEngine) -> (u64, u64, usize) {
            audio.trim_idle();
            win_sys::flush_logs();
            (0, 0, 0)
        }

        pub fn query_process_memory_bytes() -> (u64, u64) {
            (0, 0)
        }
    }

    pub fn query_memory_mb_now() -> (f64, f64) {
        let (ws, priv_b) = imp::query_process_memory_bytes();
        (format_mb(ws), format_mb(priv_b))
    }

    pub fn trim_memory_now(audio: &AudioEngine) -> String {
        let (before, after, _) = imp::force_windows_memory_reclaim(audio);
        win_sys::log_info(
            "perf",
            &format!(
                "manual memory trim requested via web console: {:.2}MB -> {:.2}MB",
                format_mb(before),
                format_mb(after)
            ),
        );
        format!(
            "已执行内存释放：{:.1} MB → {:.1} MB",
            format_mb(before),
            format_mb(after)
        )
    }

    /// 启动后台资源与性能计数器监控线程，并在超限（内存 > 50MB）时执行强制内存回收
    pub fn start_performance_monitor(
        audio: AudioEngine,
        cpu_throttled: Arc<AtomicBool>,
        latest_sample: Arc<Mutex<PerfSample>>,
    ) -> Sender<()> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let _ = std::thread::Builder::new()
            .name("perf-monitor-worker".to_string())
            .spawn(move || {
                // 延迟 10 秒初始化采样基线，避开冷启动阶段
                match stop_rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }

                let mut collector = imp::NativeCollector::new();
                let mut last_gc_at: Option<Instant> = None;
                let mut prev_overloaded = false;

                loop {
                    let sample = collector.sample();
                    if let Ok(mut guard) = latest_sample.lock() {
                        *guard = sample;
                    }
                    let overloaded = is_cpu_overloaded(&sample);
                    cpu_throttled.store(overloaded, Ordering::Relaxed);

                    if overloaded && !prev_overloaded {
                        win_sys::log_warn(
                            "perf",
                            &format!(
                                "high CPU detected ({}): proc_cpu={:.1}% sys_cpu={:.1}% (deferring non-critical background work)",
                                sample.source, sample.process_cpu_pct, sample.system_cpu_pct
                            ),
                        );
                        audio.trim_idle();
                    } else if !overloaded && prev_overloaded {
                        win_sys::log_info(
                            "perf",
                            &format!(
                                "CPU load normalized: proc_cpu={:.1}% sys_cpu={:.1}%",
                                sample.process_cpu_pct, sample.system_cpu_pct
                            ),
                        );
                    }
                    prev_overloaded = overloaded;

                    let elapsed_gc = last_gc_at.map(|t| t.elapsed());
                    match evaluate_gc_decision(
                        &sample,
                        elapsed_gc,
                        MEMORY_GC_THRESHOLD_BYTES,
                        GC_COOLDOWN,
                    ) {
                        GcDecision::MemoryExceeded => {
                            let (before_ws, after_ws, compacted) =
                                imp::force_windows_memory_reclaim(&audio);
                            last_gc_at = Some(Instant::now());
                            win_sys::log_info(
                                "perf",
                                &format!(
                                    "memory exceeded >50MB ({}), forced reclaim: ws {:.2}MB -> {:.2}MB (private={:.2}MB, heap_largest_free={}B, proc_cpu={:.1}%, sys_cpu={:.1}%)",
                                    sample.source,
                                    format_mb(before_ws),
                                    format_mb(after_ws),
                                    format_mb(sample.private_bytes),
                                    compacted,
                                    sample.process_cpu_pct,
                                    sample.system_cpu_pct
                                ),
                            );
                        }
                        GcDecision::CooldownActive => {
                            win_sys::log_debug(
                                "perf",
                                &format!(
                                    "memory above 50MB (ws={:.2}MB, private={:.2}MB) but GC cooldown active",
                                    format_mb(sample.working_set_bytes),
                                    format_mb(sample.private_bytes)
                                ),
                            );
                        }
                        GcDecision::None => {
                            win_sys::log_debug(
                                "perf",
                                &format!(
                                    "sample ({}): ws={:.2}MB private={:.2}MB proc_cpu={:.1}% sys_cpu={:.1}%",
                                    sample.source,
                                    format_mb(sample.working_set_bytes),
                                    format_mb(sample.private_bytes),
                                    sample.process_cpu_pct,
                                    sample.system_cpu_pct
                                ),
                            );
                        }
                    }
                    if let Ok(mut guard) = latest_sample.lock() {
                        *guard = sample;
                    }
                    match stop_rx.recv_timeout(PERF_SAMPLE_INTERVAL) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            });

        stop_tx
    }
}

fn build_ureq_agent_with_redirects(connect_secs: u64, read_secs: u64, redirects: u32) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(connect_secs))
        .timeout_read(Duration::from_secs(read_secs))
        .redirects(redirects);

    if let Ok(connector) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(Arc::new(connector));
    }

    let proxy_opt = std::env::var("HTTPS_PROXY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HTTP_PROXY").ok().filter(|s| !s.trim().is_empty()))
        .or_else(win_sys::read_windows_proxy_url);

    if let Some(proxy_str) = proxy_opt {
        if let Ok(proxy) = ureq::Proxy::new(&proxy_str) {
            builder = builder.proxy(proxy);
        }
    }

    builder.build()
}

fn build_ureq_agent(connect_secs: u64, read_secs: u64) -> ureq::Agent {
    build_ureq_agent_with_redirects(connect_secs, read_secs, 5)
}

fn resolve_local_or_unc_path(raw: &str) -> Option<std::path::PathBuf> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("file:///") {
        return Some(std::path::PathBuf::from(rest.replace('/', "\\")));
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        return Some(std::path::PathBuf::from(format!("\\\\{}", rest.replace('/', "\\"))));
    }
    if trimmed.starts_with("\\\\")
        || trimmed
            .as_bytes()
            .get(1)
            .map(|&b| b == b':')
            .unwrap_or(false)
    {
        return Some(std::path::PathBuf::from(trimmed));
    }
    None
}

enum HttpTextErr {
    Status(u16, String),
    Other(String),
}

fn http_get_text(agent: &ureq::Agent, url: &str, token: &str) -> Result<String, HttpTextErr> {
    let mut req = agent
        .get(url)
        .set(
            "User-Agent",
            &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
        )
        .set("Accept", "application/vnd.github+json, application/json");

    let clean_token = token.trim();
    if !clean_token.is_empty() && url.contains("api.github.com") {
        req = req.set("Authorization", &format!("Bearer {clean_token}"));
    }

    let resp = req.call().map_err(|e| match e {
        ureq::Error::Status(code, _r) => HttpTextErr::Status(code, format!("{url}: status code {code}")),
        other => HttpTextErr::Other(format!("连接更新服务器失败：{other}")),
    })?;

    let mut body = String::new();
    resp.into_reader()
        .take(256 * 1024)
        .read_to_string(&mut body)
        .map_err(|e| HttpTextErr::Other(format!("读取更新清单失败：{e}")))?;
    Ok(body)
}

fn fetch_public_github_release_via_redirect(
    agent: &ureq::Agent,
    owner: &str,
    repo: &str,
) -> Option<Result<version::RemoteVersionManifest, String>> {
    let web_latest = format!("https://github.com/{owner}/{repo}/releases/latest");
    let resp = agent
        .get(&web_latest)
        .set(
            "User-Agent",
            &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
        )
        .call()
        .ok()?;

    let final_url = resp.get_url().to_string();
    let tag_part = final_url.split("/releases/tag/").nth(1)?;
    let tag = tag_part.split(['?', '#']).next()?.trim();
    if tag.is_empty() {
        return None;
    }

    let version_json_url =
        format!("https://github.com/{owner}/{repo}/releases/download/{tag}/version.json");
    match http_get_text(agent, &version_json_url, "") {
        Ok(body) => Some(version::parse_remote_manifest_body(&body)),
        Err(_) => {
            let exe_url =
                format!("https://github.com/{owner}/{repo}/releases/download/{tag}/HalfHourChime.exe");
            Some(Ok(version::RemoteVersionManifest {
                version: tag.to_string(),
                download_url: exe_url,
                api_download_url: String::new(),
                notes: "已通过 GitHub Releases 页面检测到新版本，点击“自动更新”即可下载升级。".to_string(),
                package_type: "portable".to_string(),
            }))
        }
    }
}

/// 轻量级 HTTP/HTTPS/本地共享读取服务端版本清单（支持公开与 Token 鉴权私有 GitHub Releases、version.json、Google Drive 及局域网路径）
fn fetch_remote_manifest(url: &str, token: &str) -> Result<version::RemoteVersionManifest, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.contains("updates.zwai.local") {
        return Err(
            "尚未配置更新源地址：请在「本地 Web 控制台 → 版本与更新」填入更新地址或 GitHub Releases 地址"
                .to_string(),
        );
    }

    if let Some(local_path) = resolve_local_or_unc_path(trimmed) {
        let meta = std::fs::metadata(&local_path)
            .map_err(|e| format!("读取本地/局域网更新清单失败（{}）：{e}", local_path.display()))?;
        if meta.len() > 256 * 1024 {
            return Err("更新清单文件过大（超过 256 KB）".to_string());
        }
        let body = std::fs::read_to_string(&local_path)
            .map_err(|e| format!("读取本地/局域网更新清单失败：{e}"))?;
        return version::parse_remote_manifest_body(&body);
    }

    let resolved_url = version::normalize_update_check_url(trimmed);
    let agent = build_ureq_agent(6, 10);

    match http_get_text(&agent, &resolved_url, token) {
        Ok(body) => version::parse_remote_manifest_body(&body),
        Err(HttpTextErr::Status(code, detail)) => {
            if let Some((owner, repo)) = version::extract_github_owner_repo(trimmed) {
                if code == 404 {
                    let list_url = format!(
                        "https://api.github.com/repos/{owner}/{repo}/releases?per_page=1"
                    );
                    if let Ok(list_body) = http_get_text(&agent, &list_url, token) {
                        if let Ok(m) = version::parse_remote_manifest_body(&list_body) {
                            return Ok(m);
                        }
                    }
                    if token.trim().is_empty() {
                        if let Some(res) = fetch_public_github_release_via_redirect(&agent, &owner, &repo) {
                            return res;
                        }
                        return Err(
                            "连接更新服务器返回 404：仓库可能是私有（Private）或尚未公开发布正式 Release。请在 GitHub 将仓库改为 Public（公开），或在「本地 Web 控制台 → 版本与更新」填入只读 GitHub Token。"
                                .to_string(),
                        );
                    }
                    return Err(
                        "连接更新服务器返回 404：未找到可访问的 Release，或当前 GitHub Token 缺少该仓库的只读权限（Contents: Read）。"
                            .to_string(),
                    );
                }
                if code == 401 {
                    return Err(
                        "GitHub 访问令牌无效或已过期（401），请在「本地 Web 控制台 → 版本与更新」中重新配置。"
                            .to_string(),
                    );
                }
                if code == 403 && token.trim().is_empty() {
                    if let Some(res) = fetch_public_github_release_via_redirect(&agent, &owner, &repo) {
                        return res;
                    }
                    return Err(
                        "GitHub API 访问受限（403 请求频率限制），请稍后重试或在本地 Web 控制台配置只读 Token。"
                            .to_string(),
                    );
                }
            }
            Err(format!("连接更新服务器失败：{detail}"))
        }
        Err(HttpTextErr::Other(msg)) => Err(msg),
    }
}

/// 启动后台定期版本检查线程：空闲时零 CPU 休眠，定期对比服务端版本号并在发现新版本时提示用户下载
fn start_update_check_worker(
    initial_enabled: bool,
    initial_url: String,
    initial_token: String,
    initial_interval_hours: u16,
    main_w: &MainWindow,
    about_w: &AboutWindow,
    update_w: &UpdateWindow,
    ctx: UpdateContext,
    cpu_throttled: Arc<AtomicBool>,
) -> Sender<UpdateWorkerMsg> {
    let (tx, rx) = mpsc::channel::<UpdateWorkerMsg>();
    let mw_weak = main_w.as_weak();
    let aw_weak = about_w.as_weak();
    let uw_weak = update_w.as_weak();

    let run_check = move |url: &str, token: &str, manual: bool| {
        ctx.is_checking.store(true, Ordering::SeqCst);
        if let Ok(mut st_msg) = ctx.status_text.lock() {
            *st_msg = "正在连接更新服务器检查版本...".to_string();
        }
        let result = fetch_remote_manifest(url, token).and_then(|manifest| {
            let newer = version::is_newer_than_current(&manifest.version)?;
            Ok((newer, manifest))
        });

        let mw_w = mw_weak.clone();
        let aw_w = aw_weak.clone();
        let uw_w = uw_weak.clone();
        let ctx_clone = ctx.clone();

        ctx_clone.is_checking.store(false, Ordering::SeqCst);
        match &result {
            Ok((true, manifest)) => {
                let display_remote = if manifest.version.starts_with('v')
                    || manifest.version.starts_with('V')
                {
                    manifest.version.clone()
                } else {
                    format!("v{}", manifest.version)
                };
                if let Ok(mut guard) = ctx_clone.download_url.lock() {
                    *guard = manifest.download_url.clone();
                }
                if let Ok(mut api_guard) = ctx_clone.api_download_url.lock() {
                    *api_guard = manifest.api_download_url.clone();
                }
                if let Ok(mut pkg_guard) = ctx_clone.package_type.lock() {
                    *pkg_guard = manifest.package_type.clone();
                }
                ctx_clone.has_update.store(true, Ordering::SeqCst);
                if let Ok(mut rv) = ctx_clone.remote_version.lock() {
                    *rv = display_remote.clone();
                }
                let found_msg = format!("发现新版本 {display_remote}，可点击“自动更新”一键升级");
                if let Ok(mut st_msg) = ctx_clone.status_text.lock() {
                    *st_msg = found_msg;
                }
                let notes = if manifest.notes.trim().is_empty() {
                    "包含稳定性优化与功能改进，建议前往下载更新。".to_string()
                } else {
                    manifest.notes.clone()
                };
                if let Ok(mut n) = ctx_clone.release_notes.lock() {
                    *n = notes;
                }
            }
            Ok((false, _)) => {
                ctx_clone.has_update.store(false, Ordering::SeqCst);
                if let Ok(mut st_msg) = ctx_clone.status_text.lock() {
                    *st_msg = format!("当前已是最新版本（{}）", version::display_version());
                }
            }
            Err(err) => {
                if manual {
                    if let Ok(mut st_msg) = ctx_clone.status_text.lock() {
                        *st_msg = err.clone();
                    }
                }
            }
        }

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(aw) = aw_w.upgrade() {
                aw.set_is_checking_update(false);
            }

            match result {
                Ok((true, manifest)) => {
                    win_sys::log_info(
                        "update",
                        &format!(
                            "new version detected remote={} url={}",
                            manifest.version, manifest.download_url
                        ),
                    );
                    let display_remote = if manifest.version.starts_with('v')
                        || manifest.version.starts_with('V')
                    {
                        manifest.version.clone()
                    } else {
                        format!("v{}", manifest.version)
                    };

                    if let Some(mw) = mw_w.upgrade() {
                        mw.set_has_update(true);
                        mw.set_update_version(display_remote.clone().into());
                    }
                    let found_msg =
                        format!("发现新版本 {display_remote}，可点击“自动更新”一键升级");
                    if let Some(aw) = aw_w.upgrade() {
                        aw.set_has_update(true);
                        aw.set_update_status_text(found_msg.into());
                    }

                    // 自动后台检查时同一新版本仅主动弹窗提示一次；用户手动触发则总是弹出
                    let should_popup = if manual {
                        true
                    } else if let Ok(mut prompted) = ctx_clone.last_prompted_version.lock() {
                        if *prompted != display_remote {
                            *prompted = display_remote.clone();
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if should_popup {
                        if let Some(uw) = uw_w.upgrade() {
                            uw.set_current_version(version::display_version().into());
                            uw.set_remote_version(display_remote.into());
                            if !ctx_clone.is_downloading.load(Ordering::SeqCst) {
                                uw.set_download_progress(0);
                                uw.set_download_status_text("".into());
                            }
                            let notes = if manifest.notes.trim().is_empty() {
                                "包含稳定性优化与功能改进，建议前往下载更新。".to_string()
                            } else {
                                manifest.notes.clone()
                            };
                            center_and_show_window(&uw, 452.0, 360.0);
                            uw.set_release_notes(notes.into());
                        }
                    }
                }
                Ok((false, _)) => {
                    win_sys::log_debug("update", "version check completed: already up to date");
                    if manual {
                        let up_to_date_msg =
                            format!("当前已是最新版本（{}）", version::display_version());
                        if let Some(aw) = aw_w.upgrade() {
                            aw.set_has_update(false);
                            aw.set_update_status_text(up_to_date_msg.into());
                        }
                    }
                }
                Err(err) => {
                    win_sys::log_warn("update", &format!("version check error (manual={manual}): {err}"));
                    // 后台自动巡检遇到离线或内网未连通时保持静默，不打扰门店值守；仅在手动检查时展示具体状态
                    if manual {
                        if let Some(aw) = aw_w.upgrade() {
                            aw.set_update_status_text(err.into());
                        }
                    }
                }
            }
        });
    };

    let _ = std::thread::Builder::new()
        .name("version-check-worker".to_string())
        .spawn(move || {
            let mut enabled = initial_enabled;
            let mut url = initial_url;
            let mut token = initial_token;
            let mut interval = Duration::from_secs(initial_interval_hours.max(1) as u64 * 3600);

            // 启动后延迟 5 秒执行首次静默对比，避免影响程序冷启动速度
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(UpdateWorkerMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(UpdateWorkerMsg::CheckNow {
                    url: manual_url,
                    token: manual_token,
                    manual,
                }) => {
                    run_check(&manual_url, &manual_token, manual);
                }
                Ok(UpdateWorkerMsg::Reconfigure {
                    enabled: en,
                    url: u,
                    token: tok,
                    interval_hours: ih,
                }) => {
                    enabled = en;
                    url = u;
                    token = tok;
                    interval = Duration::from_secs(ih.max(1) as u64 * 3600);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if enabled && !url.trim().is_empty() {
                        if cpu_throttled.load(Ordering::Relaxed) {
                            win_sys::log_debug("update", "deferring initial auto-check due to high CPU load");
                        } else {
                            run_check(&url, &token, false);
                        }
                    }
                }
            }

            // 进入周期性后台检查循环
            loop {
                match rx.recv_timeout(interval) {
                    Ok(UpdateWorkerMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(UpdateWorkerMsg::CheckNow {
                        url: manual_url,
                        token: manual_token,
                        manual,
                    }) => {
                        run_check(&manual_url, &manual_token, manual);
                    }
                    Ok(UpdateWorkerMsg::Reconfigure {
                        enabled: en,
                        url: u,
                        token: tok,
                        interval_hours: ih,
                    }) => {
                        enabled = en;
                        url = u;
                        token = tok;
                        interval = Duration::from_secs(ih.max(1) as u64 * 3600);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if enabled && !url.trim().is_empty() {
                            if cpu_throttled.load(Ordering::Relaxed) {
                                win_sys::log_debug("update", "deferring periodic auto-check due to high CPU load");
                            } else {
                                run_check(&url, &token, false);
                            }
                        }
                    }
                }
            }
        });

    tx
}

fn weekday_zh(wd: Weekday) -> &'static str {
    match wd {
        Weekday::Mon => "星期一",
        Weekday::Tue => "星期二",
        Weekday::Wed => "星期三",
        Weekday::Thu => "星期四",
        Weekday::Fri => "星期五",
        Weekday::Sat => "星期六",
        Weekday::Sun => "星期日",
    }
}

fn center_window_on_work_area<T: ComponentHandle>(
    win: &T,
    fallback_logical_w: f32,
    fallback_logical_h: f32,
) {
    let w = win.window();
    if w.is_maximized() {
        return;
    }
    if let Some((left, top, right, bottom)) = win_sys::get_primary_work_area() {
        let scale = w.scale_factor().max(1.0);
        let size = w.size();
        let pw = if size.width > 0 {
            size.width as i32
        } else {
            (fallback_logical_w * scale).round() as i32
        };
        let ph = if size.height > 0 {
            size.height as i32
        } else {
            (fallback_logical_h * scale).round() as i32
        };
        let work_w = right - left;
        let work_h = bottom - top;
        let x = left + ((work_w - pw) / 2).max(0);
        let y = top + ((work_h - ph) / 2).max(0);
        w.set_position(slint::PhysicalPosition::new(x, y));
    }
}

fn apply_rounded_corners_for_window<T: ComponentHandle>(
    win: &T,
    fallback_logical_w: f32,
    fallback_logical_h: f32,
) {
    let w = win.window();
    let handle_wrapper = w.window_handle();
    if let Ok(raw_handle) = handle_wrapper.window_handle() {
        if let RawWindowHandle::Win32(win32_h) = raw_handle.as_raw() {
            let scale = w.scale_factor().max(1.0);
            let size = w.size();
            let pw = if size.width > 0 {
                size.width as i32
            } else {
                (fallback_logical_w * scale).round() as i32
            };
            let ph = if size.height > 0 {
                size.height as i32
            } else {
                (fallback_logical_h * scale).round() as i32
            };
            let radius = (8.0 * scale).round() as i32;
            win_sys::apply_rounded_window_corners(win32_h.hwnd.get(), pw, ph, radius);
        }
    }
}

fn center_and_show_window<T: ComponentHandle + 'static>(
    win: &T,
    fallback_logical_w: f32,
    fallback_logical_h: f32,
) {
    center_window_on_work_area(win, fallback_logical_w, fallback_logical_h);
    let _ = win.show();
    win.window().set_minimized(false);
    center_window_on_work_area(win, fallback_logical_w, fallback_logical_h);
    apply_rounded_corners_for_window(win, fallback_logical_w, fallback_logical_h);

    // 部分 Slint/winit 窗口在首帧事件循环完成后才完成 HWND 创建，再补一次硬件圆角确保生效
    let weak = win.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(w) = weak.upgrade() {
            apply_rounded_corners_for_window(&w, fallback_logical_w, fallback_logical_h);
        }
    });
}

fn resolve_chime_playback_params(
    cfg: &AppConfig,
    scheduler: &Scheduler,
    now: chrono::DateTime<Local>,
) -> (Option<String>, Option<String>, Option<String>) {
    let holiday = if cfg.holiday_enabled {
        scheduler.lookup_holiday(now.date_naive(), cfg)
    } else {
        None
    };
    let tts_opt = Scheduler::build_chime_announcement_with_holiday(cfg, now, holiday.as_ref());
    let holiday_audio_opt = if cfg.holiday_enabled {
        holiday.as_ref().and_then(|h| {
            h.custom_audio_path.clone().or_else(|| {
                let p = cfg.holiday_custom_audio_path.trim();
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                }
            })
        })
    } else {
        None
    };
    (holiday_audio_opt, tts_opt, holiday.map(|h| h.name))
}

fn spawn_holiday_feed_refresh(
    source: String,
    sw_weak: slint::Weak<SettingsWindow>,
    holiday_tx: Sender<(Result<Vec<ExternalHolidayEntry>, String>, bool)>,
    manual: bool,
) {
    let trimmed = source.trim().to_string();
    if trimmed.is_empty() {
        let _ = holiday_tx.send((Ok(Vec::new()), manual));
        if manual {
            if let Some(sw) = sw_weak.upgrade() {
                sw.set_holiday_feed_status(
                    "未配置外部节假日接口，当前仅使用内置农历与自定义节日规则。".into(),
                );
            }
        }
        return;
    }

    if manual {
        if let Some(sw) = sw_weak.upgrade() {
            sw.set_holiday_feed_status("正在拉取节假日数据接口...".into());
        }
    }

    let _ = std::thread::Builder::new()
        .name("holiday-feed-worker".to_string())
        .spawn(move || {
            let res = fetch_holiday_feed_from_source(&trimmed);
            let status_text = match &res {
                Ok(entries) => {
                    win_sys::log_info(
                        "holiday",
                        &format!("holiday feed sync succeeded entries={}", entries.len()),
                    );
                    format!("节假日接口同步成功：已加载 {} 条规则。", entries.len())
                }
                Err(err) => {
                    win_sys::log_warn("holiday", &format!("holiday feed sync failed: {err}"));
                    format!("拉取失败（仍保留内置农历库）：{err}")
                }
            };
            let _ = holiday_tx.send((res, manual));
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(sw) = sw_weak.upgrade() {
                    sw.set_holiday_feed_status(status_text.into());
                }
            });
        });
}

fn build_meal_tts_text(
    enabled: bool,
    text: &str,
    now: chrono::DateTime<Local>,
) -> Option<String> {
    Scheduler::build_meal_announcement(enabled, text, now)
}

fn drag_slint_window<T: ComponentHandle>(win: &T, dx: f32, dy: f32) {
    let w = win.window();
    if w.is_maximized() {
        return;
    }
    let scale = w.scale_factor().max(1.0);
    let pos = w.position();
    w.set_position(slint::PhysicalPosition::new(
        pos.x + (dx * scale).round() as i32,
        pos.y + (dy * scale).round() as i32,
    ));
}

fn sync_theme_across_windows(
    main_w: &MainWindow,
    settings_w: &SettingsWindow,
    about_w: &AboutWindow,
    update_w: &UpdateWindow,
    theme_idx: i32,
) {
    ThemeStore::get(main_w).set_active_index(theme_idx);
    ThemeStore::get(settings_w).set_active_index(theme_idx);
    ThemeStore::get(about_w).set_active_index(theme_idx);
    ThemeStore::get(update_w).set_active_index(theme_idx);
}

fn populate_settings_form(
    settings_w: &SettingsWindow,
    cfg: &AppConfig,
    scheduler: &Scheduler,
) {
    settings_w.set_chime_minutes_str(cfg.format_chime_minutes().into());
    settings_w.set_chime_repeat(cfg.chime_repeat_count as i32);
    settings_w.set_chime_play_sound(cfg.chime_play_sound);
    settings_w.set_chime_tts_enabled(cfg.chime_tts_enabled);
    settings_w.set_chime_tts_template(cfg.chime_tts_template.clone().into());

    settings_w.set_holiday_enabled(cfg.holiday_enabled);
    settings_w.set_holiday_custom_audio_path(cfg.holiday_custom_audio_path.clone().into());
    settings_w.set_holiday_greeting_template(cfg.holiday_greeting_template.clone().into());
    settings_w.set_holiday_custom_rules(cfg.holiday_custom_rules.clone().into());
    settings_w.set_holiday_data_source_url(cfg.holiday_data_source_url.clone().into());
    settings_w.set_holiday_today_preview(
        scheduler
            .describe_day_calendar(Local::now().date_naive(), cfg)
            .into(),
    );
    let feed_status = if scheduler.external_holiday_count() > 0 {
        format!("当前已加载 {} 条外部节假日规则。", scheduler.external_holiday_count())
    } else {
        String::new()
    };
    settings_w.set_holiday_feed_status(feed_status.into());

    settings_w.set_quiet_enabled(cfg.quiet_enabled);
    settings_w.set_quiet_start_hour(cfg.quiet_start_hour as i32);
    settings_w.set_quiet_start_minute(cfg.quiet_start_minute as i32);
    settings_w.set_quiet_end_hour(cfg.quiet_end_hour as i32);
    settings_w.set_quiet_end_minute(cfg.quiet_end_minute as i32);

    settings_w.set_meal_enabled(cfg.meal_enabled);
    settings_w.set_meal_hour(cfg.meal_hour as i32);
    settings_w.set_meal_minute(cfg.meal_minute as i32);
    settings_w.set_meal_repeat(cfg.meal_repeat_count as i32);
    settings_w.set_meal_custom_audio_path(cfg.meal_custom_audio_path.clone().into());
    settings_w.set_meal_play_chime_before_custom(cfg.meal_play_chime_before_custom);
    settings_w.set_meal_tts_enabled(cfg.meal_tts_enabled);
    settings_w.set_meal_tts_text(cfg.meal_tts_text.clone().into());
    settings_w.set_tts_engine_index(cfg.tts_engine.to_index());
    settings_w.set_edge_voice_index(edge_voice_to_index(&cfg.edge_tts_voice));
    settings_w.set_tts_rate(cfg.tts_rate as i32);
    settings_w.set_tts_preview_status("".into());

    settings_w.set_volume(cfg.volume as i32);
    settings_w.set_ntp_server(cfg.ntp_server.clone().into());
    settings_w.set_auto_start(cfg.auto_start);
    settings_w.set_theme_index(cfg.theme.to_index());
    settings_w.set_web_console_enabled(cfg.web_console_enabled);
    settings_w.set_web_console_url(DEFAULT_WEB_CONSOLE_URL.into());
    settings_w.set_web_console_status(
        if cfg.web_console_enabled {
            format!("已启用：点击右侧“打开网页”或访问 {DEFAULT_WEB_CONSOLE_URL}")
        } else {
            "未启用（本地 3000 端口处于关闭状态）".to_string()
        }
        .into(),
    );
    settings_w.set_validation_error("".into());
}

fn refresh_main_window_ui(main_w: &MainWindow, state: &AppState) {
    let now = Local::now();
    let today = now.date_naive();
    let time_str = now.format("%H:%M:%S").to_string();
    let lunar_part = solar_to_lunar(today)
        .map(|l| format!(" · {}", l.format_zh()))
        .unwrap_or_default();
    let holiday_part = if state.config.holiday_enabled {
        state
            .scheduler
            .lookup_holiday(today, &state.config)
            .map(|h| format!(" · 节庆：{}", h.name))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let date_str = format!(
        "{} · {}{}{}",
        now.format("%Y-%m-%d"),
        weekday_zh(now.weekday()),
        lunar_part,
        holiday_part
    );

    let is_quiet = state.config.mute_mode
        || Scheduler::is_quiet_at(
            &state.config,
            chrono::Timelike::hour(&now),
            chrono::Timelike::minute(&now),
        );
    let quiet_badge = if state.config.mute_mode {
        "静音模式（托盘已开启）".to_string()
    } else if state.config.quiet_enabled {
        format!(
            "静默 {:02}:{:02}–{:02}:{:02}",
            state.config.quiet_start_hour,
            state.config.quiet_start_minute,
            state.config.quiet_end_hour,
            state.config.quiet_end_minute
        )
    } else {
        "未启用静默".to_string()
    };

    let next = Scheduler::compute_next_event(&state.config, now);
    let summary = format!("报时分钟：{}", state.config.format_chime_minutes());

    main_w.set_current_time(time_str.into());
    main_w.set_current_date(date_str.into());
    main_w.set_is_quiet_now(is_quiet);
    main_w.set_quiet_badge_text(quiet_badge.into());
    main_w.set_next_event_title(next.title.into());
    main_w.set_next_event_time(next.target_time_text.into());
    main_w.set_next_event_countdown(next.countdown_text.into());
    main_w.set_chime_summary_text(summary.into());
    main_w.set_ntp_status_code(state.ntp_status_code);
    main_w.set_ntp_status_text(state.ntp_status_text.clone().into());
    main_w.set_is_syncing(state.is_syncing);
}

fn trigger_ntp_sync(
    state_rc: &Rc<RefCell<AppState>>,
    main_w: &MainWindow,
    settings_w: &SettingsWindow,
    server_override: Option<String>,
) {
    let server = {
        let mut st = state_rc.borrow_mut();
        if st.is_syncing {
            return;
        }
        st.is_syncing = true;
        st.ntp_status_code = 1;
        let target = server_override.unwrap_or_else(|| st.config.ntp_server.clone());
        st.ntp_status_text = format!("正在连接 NTP 服务器：{target} ...");
        refresh_main_window_ui(main_w, &st);
        settings_w.set_is_syncing(true);
        target
    };

    let state_weak_tx = main_w.as_weak();
    let settings_weak_tx = settings_w.as_weak();

    win_sys::log_info("ntp", &format!("starting NTP sync server={server}"));
    std::thread::Builder::new()
        .name("ntp-sync-worker".to_string())
        .spawn(move || {
            let result = ntp::sync_system_time_with_elevation(&server);
            if result.success {
                win_sys::log_info(
                    "ntp",
                    &format!("NTP sync succeeded at={} msg={}", result.synced_at, result.message),
                );
            } else {
                win_sys::log_warn(
                    "ntp",
                    &format!("NTP sync failed at={} msg={}", result.synced_at, result.message),
                );
            }
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(mw) = state_weak_tx.upgrade() {
                    mw.set_is_syncing(false);
                    if result.success {
                        mw.set_ntp_status_code(2);
                        mw.set_ntp_status_text(
                            format!("校时成功 · {} · {}", result.synced_at, result.message).into(),
                        );
                    } else {
                        mw.set_ntp_status_code(3);
                        mw.set_ntp_status_text(
                            format!("校时失败 · {} · {}", result.synced_at, result.message).into(),
                        );
                    }
                }
                if let Some(sw) = settings_weak_tx.upgrade() {
                    sw.set_is_syncing(false);
                }
            });
        });
}

fn trigger_manual_update_check(
    update_tx: &Sender<UpdateWorkerMsg>,
    about_w: &AboutWindow,
    url: String,
    token: String,
) {
    let target = if url.trim().is_empty() {
        version::DEFAULT_GITHUB_RELEASE_URL.to_string()
    } else {
        url
    };
    about_w.set_is_checking_update(true);
    about_w.set_update_status_text("正在检查服务端版本...".into());
    let _ = update_tx.send(UpdateWorkerMsg::CheckNow {
        url: target,
        token,
        manual: true,
    });
}

fn open_downloaded_update(ctx: &UpdateContext, about_w: &AboutWindow) {
    if let Ok(guard) = ctx.download_url.lock() {
        let url = guard.trim();
        if !url.is_empty() {
            if let Err(err) = win_sys::open_url_in_browser(url) {
                about_w.set_update_status_text(err.into());
            }
        }
    }
}

/// 执行“自动更新”：流式下载临时安装包 -> 校验 PE 可执行头部 -> 拉起分离升级脚本 -> 释放单实例锁并重启
fn download_and_apply_auto_update(
    ctx: &UpdateContext,
    update_w: &UpdateWindow,
    update_tx: &Sender<UpdateWorkerMsg>,
    perf_stop_tx: &Sender<()>,
    audio: &AudioEngine,
    single_instance_guard: &Rc<RefCell<Option<win_sys::SingleInstanceGuard>>>,
) {
    let raw_url = ctx
        .download_url
        .lock()
        .map(|g| g.trim().to_string())
        .unwrap_or_default();
    let api_asset_url = ctx
        .api_download_url
        .lock()
        .map(|g| g.trim().to_string())
        .unwrap_or_default();
    let gh_token = ctx
        .github_token
        .lock()
        .map(|g| g.trim().to_string())
        .unwrap_or_default();
    let local_src = resolve_local_or_unc_path(&raw_url);
    let url = version::normalize_download_url(&raw_url);
    if local_src.is_none()
        && (url.is_empty() || (!url.starts_with("https://") && !url.starts_with("http://")))
    {
        update_w.set_download_status_text("下载地址无效，请检查服务端更新清单。".into());
        center_and_show_window(update_w, 452.0, 360.0);
        return;
    }

    if ctx
        .is_downloading
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        center_and_show_window(update_w, 452.0, 360.0);
        return;
    }

    let is_installer_mode = ctx
        .package_type
        .lock()
        .map(|g| g.trim().eq_ignore_ascii_case("installer"))
        .unwrap_or(false);

    update_w.set_is_downloading(true);
    update_w.set_download_progress(0);
    update_w.set_download_status_text("正在连接更新源并准备下载安装包...".into());
    ctx.download_progress.store(0, Ordering::SeqCst);
    if let Ok(mut s) = ctx.download_status_text.lock() {
        *s = "正在连接更新源并准备下载安装包...".to_string();
    }

    let uw_weak = update_w.as_weak();
    center_and_show_window(update_w, 452.0, 360.0);
    let ctx_clone = ctx.clone();
    let u_tx_clone = update_tx.clone();
    let perf_tx_clone = perf_stop_tx.clone();
    let audio_clone = audio.clone();
    // 使用通道通知主线程执行优雅退出与平滑重启（确保在主线程释放 Rc<RefCell<SingleInstanceGuard>>）
    let (restart_tx, restart_rx) = mpsc::channel::<()>();
    {
        let guard_rc = Rc::clone(single_instance_guard);
        let u_tx_exit = u_tx_clone.clone();
        let perf_tx_exit = perf_tx_clone.clone();
        let audio_exit = audio_clone.clone();
        let restart_poll = Timer::default();
        restart_poll.start(TimerMode::Repeated, Duration::from_millis(80), move || {
            if restart_rx.try_recv().is_ok() {
                let _ = u_tx_exit.send(UpdateWorkerMsg::Shutdown);
                let _ = perf_tx_exit.send(());
                audio_exit.shutdown();
                // 必须在退出前释放单实例互斥锁，防止重启后的新版本被误判为重复启动
                let _ = guard_rc.borrow_mut().take();
                let _ = slint::quit_event_loop();
            }
        });
        std::mem::forget(restart_poll);
    }

    win_sys::log_info(
        "updater",
        &format!("starting auto-update download url={url} installer_mode={is_installer_mode}"),
    );

    let _ = std::thread::Builder::new()
        .name("auto-update-worker".to_string())
        .spawn(move || {
            let download_result = (|| -> Result<std::path::PathBuf, String> {
                const MAX_PACKAGE_BYTES: u64 = 100 * 1024 * 1024;
                let pid = std::process::id();
                let part_path =
                    std::env::temp_dir().join(format!("HalfHourChime_update_{pid}.exe.part"));
                let final_path =
                    std::env::temp_dir().join(format!("HalfHourChime_update_{pid}.exe"));

                if let Some(src_path) = local_src {
                    let meta = std::fs::metadata(&src_path).map_err(|e| {
                        format!("读取本地/局域网更新包失败（{}）：{e}", src_path.display())
                    })?;
                    if meta.len() > MAX_PACKAGE_BYTES {
                        return Err("更新包体积超出安全上限（100 MB），已中止更新。".to_string());
                    }
                    std::fs::copy(&src_path, &part_path)
                        .map_err(|e| format!("复制本地/局域网更新包失败：{e}"))?;
                    let bytes = std::fs::read(&part_path)
                        .map_err(|e| format!("校验临时安装包失败：{e}"))?;
                    if bytes.len() < 64 * 1024 || &bytes[..2] != b"MZ" {
                        let _ = std::fs::remove_file(&part_path);
                        return Err("目标文件不是有效的 Windows 可执行程序（.exe）。".to_string());
                    }
                } else {
                    let agent = build_ureq_agent(6, 60);

                    let resp = if !gh_token.is_empty() && !api_asset_url.is_empty() {
                        let no_redir = build_ureq_agent_with_redirects(6, 60, 0);
                        let api_resp = no_redir
                            .get(&api_asset_url)
                            .set(
                                "User-Agent",
                                &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
                            )
                            .set("Accept", "application/octet-stream")
                            .set("Authorization", &format!("Bearer {gh_token}"))
                            .call()
                            .map_err(|e| format!("请求私有仓库安装包地址失败：{e}"))?;

                        if (300..400).contains(&api_resp.status()) {
                            let signed_url = api_resp
                                .header("Location")
                                .ok_or_else(|| "GitHub 资产重定向缺少 Location 头".to_string())?
                                .to_string();
                            agent
                                .get(&signed_url)
                                .set(
                                    "User-Agent",
                                    &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
                                )
                                .call()
                                .map_err(|e| format!("下载更新包失败：{e}"))?
                        } else {
                            api_resp
                        }
                    } else {
                        agent
                            .get(&url)
                            .set(
                                "User-Agent",
                                &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
                            )
                            .call()
                            .map_err(|e| format!("下载更新包失败：{e}"))?
                    };

                    let total_bytes: Option<u64> = resp
                        .header("Content-Length")
                        .and_then(|v| v.parse::<u64>().ok());

                    if let Some(len) = total_bytes {
                        if len > MAX_PACKAGE_BYTES {
                            return Err(
                                "更新包体积超出安全上限（100 MB），已中止下载。".to_string()
                            );
                        }
                    }

                    let mut file = std::fs::File::create(&part_path)
                        .map_err(|e| format!("创建临时安装包文件失败：{e}"))?;

                    let mut reader = resp.into_reader();
                    let mut buf = [0u8; 16 * 1024];
                    let mut downloaded: u64 = 0;
                    let mut header_bytes = Vec::with_capacity(2);
                    let mut last_reported_pct: i32 = -1;
                    let mut last_reported_mb: u64 = 0;

                    loop {
                        let n = reader
                            .read(&mut buf)
                            .map_err(|e| format!("读取安装包数据流中断：{e}"))?;
                        if n == 0 {
                            break;
                        }
                        downloaded += n as u64;
                        if downloaded > MAX_PACKAGE_BYTES {
                            let _ = std::fs::remove_file(&part_path);
                            return Err("更新包体积超出安全上限，已中止下载。".to_string());
                        }
                        if header_bytes.len() < 2 {
                            let need = 2 - header_bytes.len();
                            header_bytes.extend_from_slice(&buf[..need.min(n)]);
                        }
                        file.write_all(&buf[..n])
                            .map_err(|e| format!("写入临时安装包失败：{e}"))?;

                        if let Some(total) = total_bytes.filter(|&t| t > 0) {
                            let pct = ((downloaded * 100) / total).min(100) as i32;
                            if pct >= last_reported_pct + 2 || pct == 100 {
                                last_reported_pct = pct;
                                let status_str = format!("正在下载更新包（{pct}%）...");
                                ctx_clone.download_progress.store(pct, Ordering::SeqCst);
                                if let Ok(mut ds) = ctx_clone.download_status_text.lock() {
                                    *ds = status_str.clone();
                                }
                                let uw_w = uw_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(uw) = uw_w.upgrade() {
                                        uw.set_download_progress(pct);
                                        uw.set_download_status_text(status_str.into());
                                    }
                                });
                            }
                        } else {
                            let quarter_mb = downloaded / (256 * 1024);
                            if quarter_mb > last_reported_mb {
                                last_reported_mb = quarter_mb;
                                let mb = downloaded as f64 / (1024.0 * 1024.0);
                                let status_str = format!("正在下载更新包（已下载 {mb:.2} MB）...");
                                ctx_clone.download_progress.store(50, Ordering::SeqCst);
                                if let Ok(mut ds) = ctx_clone.download_status_text.lock() {
                                    *ds = status_str.clone();
                                }
                                let uw_w = uw_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(uw) = uw_w.upgrade() {
                                        uw.set_download_progress(50);
                                        uw.set_download_status_text(status_str.into());
                                    }
                                });
                            }
                        }
                    }

                    file.flush()
                        .map_err(|e| format!("刷新临时安装包缓冲区失败：{e}"))?;
                    drop(file);

                    // 基础完整性与 PE 可执行文件头校验（防止误下 HTML 错误页）
                    if downloaded < 64 * 1024 || header_bytes.as_slice() != b"MZ" {
                        let _ = std::fs::remove_file(&part_path);
                        return Err(
                            "下载的文件不是有效的 Windows 可执行更新包（可能返回了网页拦截页），请改用浏览器下载或检查直链。"
                                .to_string(),
                        );
                    }
                }

                if std::fs::rename(&part_path, &final_path).is_err() {
                    let _ = std::fs::remove_file(&final_path);
                    std::fs::rename(&part_path, &final_path)
                        .map_err(|e| format!("准备临时安装包失败：{e}"))?;
                }

                Ok(final_path)
            })();

            match download_result {
                Ok(temp_pkg_path) => {
                    let uw_w = uw_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(uw) = uw_w.upgrade() {
                            uw.set_download_progress(100);
                            uw.set_download_status_text("下载完成，正在重启并应用新版本...".into());
                        }
                    });

                    if let Err(err) =
                        win_sys::spawn_detached_updater_and_restart(&temp_pkg_path, is_installer_mode)
                    {
                        let _ = std::fs::remove_file(&temp_pkg_path);
                        ctx_clone.is_downloading.store(false, Ordering::SeqCst);
                        let uw_w2 = uw_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(uw) = uw_w2.upgrade() {
                                uw.set_is_downloading(false);
                                uw.set_download_status_text(err.into());
                            }
                        });
                        return;
                    }

                    // 通知主线程释放单实例锁并退出，由分离式升级器替换文件并自动重启
                    win_sys::log_info(
                        "updater",
                        &format!(
                            "temp package verified at \"{}\", signaling graceful restart",
                            temp_pkg_path.display()
                        ),
                    );
                    win_sys::flush_logs();
                    let _ = restart_tx.send(());
                }
                Err(err) => {
                    win_sys::log_error("updater", &format!("auto-update download failed: {err}"));
                    ctx_clone.is_downloading.store(false, Ordering::SeqCst);
                    if let Ok(mut ds) = ctx_clone.download_status_text.lock() {
                        *ds = err.clone();
                    }
                    let uw_w = uw_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(uw) = uw_w.upgrade() {
                            uw.set_is_downloading(false);
                            uw.set_download_progress(0);
                            uw.set_download_status_text(err.into());
                        }
                    });
                }
            }
        });
}

fn config_to_web_view(cfg: &AppConfig) -> WebConfigView {
    WebConfigView {
        chime_minutes_str: cfg.format_chime_minutes(),
        chime_repeat_count: cfg.chime_repeat_count,
        chime_play_sound: cfg.chime_play_sound,
        chime_tts_enabled: cfg.chime_tts_enabled,
        chime_tts_template: cfg.chime_tts_template.clone(),
        holiday_enabled: cfg.holiday_enabled,
        holiday_greeting_template: cfg.holiday_greeting_template.clone(),
        holiday_custom_rules: cfg.holiday_custom_rules.clone(),
        holiday_data_source_url: cfg.holiday_data_source_url.clone(),
        holiday_custom_audio_path: cfg.holiday_custom_audio_path.clone(),
        quiet_enabled: cfg.quiet_enabled,
        quiet_start_hour: cfg.quiet_start_hour,
        quiet_start_minute: cfg.quiet_start_minute,
        quiet_end_hour: cfg.quiet_end_hour,
        quiet_end_minute: cfg.quiet_end_minute,
        meal_enabled: cfg.meal_enabled,
        meal_hour: cfg.meal_hour,
        meal_minute: cfg.meal_minute,
        meal_repeat_count: cfg.meal_repeat_count,
        meal_play_chime_before_custom: cfg.meal_play_chime_before_custom,
        meal_custom_audio_path: cfg.meal_custom_audio_path.clone(),
        meal_tts_enabled: cfg.meal_tts_enabled,
        meal_tts_text: cfg.meal_tts_text.clone(),
        tts_engine_index: cfg.tts_engine.to_index(),
        edge_voice_index: edge_voice_to_index(&cfg.edge_tts_voice),
        tts_rate: cfg.tts_rate,
        volume: cfg.volume,
        ntp_server: cfg.ntp_server.clone(),
        auto_start: cfg.auto_start,
        theme_index: cfg.theme.to_index(),
        update_check_url: cfg.update_check_url.clone(),
        update_check_interval_hours: cfg.update_check_interval_hours,
        has_github_token: !cfg.update_github_token.trim().is_empty(),
    }
}

fn apply_web_config_patch(base: &AppConfig, patch: WebConfigPatch) -> Result<AppConfig, String> {
    let mut next = base.clone();
    if let Some(ref mins) = patch.chime_minutes_str {
        next.chime_minutes = AppConfig::parse_chime_minutes(mins)?;
    }
    if let Some(v) = patch.chime_repeat_count {
        next.chime_repeat_count = v;
    }
    if let Some(v) = patch.chime_play_sound {
        next.chime_play_sound = v;
    }
    if let Some(v) = patch.chime_tts_enabled {
        next.chime_tts_enabled = v;
    }
    if let Some(v) = patch.chime_tts_template {
        next.chime_tts_template = v;
    }
    if let Some(v) = patch.holiday_enabled {
        next.holiday_enabled = v;
    }
    if let Some(v) = patch.holiday_greeting_template {
        next.holiday_greeting_template = v;
    }
    if let Some(v) = patch.holiday_custom_rules {
        next.holiday_custom_rules = v;
    }
    if let Some(v) = patch.holiday_data_source_url {
        next.holiday_data_source_url = v;
    }
    if let Some(v) = patch.holiday_custom_audio_path {
        next.holiday_custom_audio_path = v;
    }
    if let Some(v) = patch.quiet_enabled {
        next.quiet_enabled = v;
    }
    if let Some(v) = patch.quiet_start_hour {
        next.quiet_start_hour = v;
    }
    if let Some(v) = patch.quiet_start_minute {
        next.quiet_start_minute = v;
    }
    if let Some(v) = patch.quiet_end_hour {
        next.quiet_end_hour = v;
    }
    if let Some(v) = patch.quiet_end_minute {
        next.quiet_end_minute = v;
    }
    if let Some(v) = patch.meal_enabled {
        next.meal_enabled = v;
    }
    if let Some(v) = patch.meal_hour {
        next.meal_hour = v;
    }
    if let Some(v) = patch.meal_minute {
        next.meal_minute = v;
    }
    if let Some(v) = patch.meal_repeat_count {
        next.meal_repeat_count = v;
    }
    if let Some(v) = patch.meal_play_chime_before_custom {
        next.meal_play_chime_before_custom = v;
    }
    if let Some(v) = patch.meal_custom_audio_path {
        next.meal_custom_audio_path = v;
    }
    if let Some(v) = patch.meal_tts_enabled {
        next.meal_tts_enabled = v;
    }
    if let Some(v) = patch.meal_tts_text {
        next.meal_tts_text = v;
    }
    if let Some(v) = patch.tts_engine_index {
        next.tts_engine = TtsEngine::from_index(v);
    }
    if let Some(v) = patch.edge_voice_index {
        next.edge_tts_voice = edge_voice_from_index(v).to_string();
    }
    if let Some(v) = patch.tts_rate {
        next.tts_rate = v;
    }
    if let Some(v) = patch.volume {
        next.volume = v.min(100);
    }
    if let Some(v) = patch.ntp_server {
        next.ntp_server = v;
    }
    if let Some(v) = patch.auto_start {
        next.auto_start = v;
    }
    if let Some(v) = patch.theme_index {
        next.theme = ThemeMode::from_index(v);
    }
    if let Some(v) = patch.update_check_url {
        next.update_check_url = v;
    }
    if let Some(v) = patch.update_check_interval_hours {
        next.update_check_interval_hours = v;
    }
    if patch.clear_github_token == Some(true) {
        next.update_github_token.clear();
    } else if let Some(tok) = patch.update_github_token {
        if !tok.trim().is_empty() {
            next.update_github_token = tok;
        }
    }
    next.validate()?;
    Ok(next)
}

fn format_uptime(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let rem_secs = secs % 60;
    if hours > 0 {
        format!("{hours}小时{mins}分")
    } else if mins > 0 {
        format!("{mins}分{rem_secs:02}秒")
    } else {
        format!("{rem_secs}秒")
    }
}

fn main() {
    // 0. 检查是否为按需 UAC 提权校时子进程模式（--sync-ntp <server>）
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--sync-ntp") {
        let server = args
            .get(pos + 1)
            .map(|s| s.as_str())
            .unwrap_or("ntp.aliyun.com");
        let code = ntp::run_elevated_cli_mode(server);
        std::process::exit(code);
    }

    // 1. 单实例互斥锁检查（FR-24 / AC-09），支持自动更新重启前主动释放
    let single_instance_guard = Rc::new(RefCell::new(match win_sys::acquire_single_instance() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            return;
        }
        Err(_) => None,
    }));

    // 2. 加载或初始化本地结构化配置（3.1 / 4.2），并初始化按大小自动轮转的调试日志器
    let config_store = ConfigStore::resolve();
    win_sys::init_logger(config_store.path.parent());
    win_sys::log_environment_snapshot(&version::display_version(), &config_store.path);

    let (cfg, load_warning) = config_store.load_or_init();
    if let Some(ref warn_msg) = load_warning {
        win_sys::log_warn("config", &format!("config load warning: {warn_msg}"));
    }

    // 3. 同步开机自启状态至系统注册表（3.1 步骤 4）
    if cfg.auto_start != win_sys::query_auto_start_enabled() {
        let _ = win_sys::apply_auto_start(cfg.auto_start);
    }

    // 4. 启动音频播放引擎（内嵌 WAV 资源）并接入实时语音试听状态通道
    let audio = AudioEngine::start();
    let (tts_status_tx, tts_status_rx) = mpsc::channel::<(u64, String)>();
    audio.attach_tts_status_sender(tts_status_tx);

    // 5. 构建 Slint 原生多窗口组件（默认隐藏，仅托盘可见）
    let main_win = MainWindow::new().expect("Failed to create MainWindow");
    let settings_win = SettingsWindow::new().expect("Failed to create SettingsWindow");
    let about_win = AboutWindow::new().expect("Failed to create AboutWindow");
    let update_win = UpdateWindow::new().expect("Failed to create UpdateWindow");

    about_win.set_app_version(version::display_version().into());
    about_win.set_app_vendor(version::APP_VENDOR.into());
    about_win.set_config_path(config_store.path.to_string_lossy().to_string().into());
    update_win.set_current_version(version::display_version().into());
    sync_theme_across_windows(
        &main_win,
        &settings_win,
        &about_win,
        &update_win,
        cfg.theme.to_index(),
    );

    let initial_ntp_text = load_warning.unwrap_or_else(|| "校时状态：尚未校时".to_string());
    let initial_volume = cfg.volume;
    let initial_auto_start = cfg.auto_start;
    let initial_mute_mode = cfg.mute_mode;
    let initial_holiday_url = cfg.holiday_data_source_url.clone();
    let initial_update_enabled = cfg.update_check_enabled;
    let initial_update_url = if cfg.update_check_url.trim().is_empty() {
        version::DEFAULT_GITHUB_RELEASE_URL.to_string()
    } else {
        cfg.update_check_url.clone()
    };
    let initial_update_token = cfg.update_github_token.clone();
    let initial_update_interval = cfg.update_check_interval_hours;
    let app_started_at = Instant::now();

    let state = Rc::new(RefCell::new(AppState {
        config_store,
        config: cfg,
        live_volume: initial_volume,
        scheduler: Scheduler::new(),
        is_syncing: false,
        ntp_status_code: 0,
        ntp_status_text: initial_ntp_text,
        holiday_feed_status: "使用内置公历/农历节日表".to_string(),
        tts_preview_status: String::new(),
    }));

    refresh_main_window_ui(&main_win, &state.borrow());

    let (holiday_tx, holiday_rx) =
        mpsc::channel::<(Result<Vec<ExternalHolidayEntry>, String>, bool)>();
    if !initial_holiday_url.trim().is_empty() {
        spawn_holiday_feed_refresh(
            initial_holiday_url,
            settings_win.as_weak(),
            holiday_tx.clone(),
            false,
        );
    }

    // 5.5 启动后台性能计数器监控与定期网络版本检查工作线程
    let cpu_throttled = Arc::new(AtomicBool::new(false));
    let latest_perf = Arc::new(Mutex::new(perf_guard::PerfSample::default()));
    let perf_stop_tx = perf_guard::start_performance_monitor(
        audio.clone(),
        Arc::clone(&cpu_throttled),
        Arc::clone(&latest_perf),
    );
    let update_ctx = UpdateContext::default();
    if let Ok(mut tok_guard) = update_ctx.github_token.lock() {
        *tok_guard = initial_update_token.clone();
    }
    if let Ok(mut st_msg) = update_ctx.status_text.lock() {
        *st_msg = "已启用 GitHub Releases 自动更新检查".to_string();
    }
    let update_tx = start_update_check_worker(
        initial_update_enabled,
        initial_update_url,
        initial_update_token,
        initial_update_interval,
        &main_win,
        &about_win,
        &update_win,
        update_ctx.clone(),
        Arc::clone(&cpu_throttled),
    );

    // 5.6 初始化可选本地 Web 控制台（端口 3000，默认关闭，仅当用户启用时监听 127.0.0.1:3000）
    let (web_action_tx, web_action_rx) = mpsc::channel::<WebAction>();
    let web_console = Rc::new(RefCell::new(WebConsoleController::new(web_action_tx)));
    web_console
        .borrow()
        .update_config_view(config_to_web_view(&state.borrow().config));
    if state.borrow().config.web_console_enabled {
        if let Err(err) = web_console.borrow_mut().start(DEFAULT_WEB_CONSOLE_PORT) {
            win_sys::log_warn("web", &format!("failed to auto-start web console: {err}"));
        }
    }

    // 6. 初始化系统托盘图标与右键上下文菜单（FR-14 / FR-15 / 运维快捷菜单）
    let tray = Arc::new(
        TrayController::create(initial_auto_start, initial_mute_mode)
            .expect("Failed to initialize system tray"),
    );

    // 7. 窗口关闭行为拦截：关闭主窗仅最小化到托盘，不退出进程（FR-17）
    {
        let mw_weak = main_win.as_weak();
        main_win.window().on_close_requested(move || {
            if let Some(mw) = mw_weak.upgrade() {
                let _ = mw.hide();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let audio_clone = audio.clone();
        settings_win.window().on_close_requested(move || {
            let st = st_rc.borrow();
            audio_clone.set_live_volume(st.config.volume);
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, st.config.theme.to_index());
                let _ = sw.hide();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    {
        let aw_weak = about_win.as_weak();
        about_win.window().on_close_requested(move || {
            if let Some(aw) = aw_weak.upgrade() {
                let _ = aw.hide();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    {
        let uw_weak = update_win.as_weak();
        let ctx_clone = update_ctx.clone();
        update_win.window().on_close_requested(move || {
            if !ctx_clone.is_downloading.load(Ordering::SeqCst) {
                if let Some(uw) = uw_weak.upgrade() {
                    let _ = uw.hide();
                }
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    // 7.5 绑定四个无边框窗口的自定义标题栏拖拽、最小化与关闭按钮控制
    macro_rules! bind_custom_titlebar {
        ($win:expr) => {{
            let w_drag = $win.as_weak();
            $win.on_drag_window(move |dx, dy| {
                if let Some(w) = w_drag.upgrade() {
                    drag_slint_window(&w, dx, dy);
                }
            });
            let w_min = $win.as_weak();
            $win.on_minimize_window(move || {
                if let Some(w) = w_min.upgrade() {
                    w.window().set_minimized(true);
                }
            });
        }};
    }
    bind_custom_titlebar!(main_win);
    bind_custom_titlebar!(settings_win);
    bind_custom_titlebar!(about_win);
    bind_custom_titlebar!(update_win);

    {
        let mw_weak = main_win.as_weak();
        main_win.on_close_clicked(move || {
            if let Some(mw) = mw_weak.upgrade() {
                let _ = mw.hide();
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let audio_clone = audio.clone();
        settings_win.on_close_clicked(move || {
            let mut st = st_rc.borrow_mut();
            st.live_volume = st.config.volume;
            audio_clone.set_live_volume(st.config.volume);
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, st.config.theme.to_index());
                let _ = sw.hide();
            }
        });
    }
    {
        let uw_weak = update_win.as_weak();
        let ctx_clone = update_ctx.clone();
        update_win.on_close_clicked(move || {
            if !ctx_clone.is_downloading.load(Ordering::SeqCst) {
                if let Some(uw) = uw_weak.upgrade() {
                    let _ = uw.hide();
                }
            }
        });
    }

    // 8. 注册单实例跨进程唤醒监听（FR-24 / AC-09）
    let ipc_wake_pending = Arc::new(AtomicBool::new(false));
    {
        let wake_flag = Arc::clone(&ipc_wake_pending);
        win_sys::start_ipc_wake_listener(move || {
            wake_flag.store(true, Ordering::SeqCst);
        });
    }

    // 9. 绑定主窗口按钮与更新横幅回调
    {
        let mw_weak = main_win.as_weak();
        main_win.on_minimize_to_tray(move || {
            if let Some(mw) = mw_weak.upgrade() {
                let _ = mw.hide();
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let audio_clone = audio.clone();
        main_win.on_test_chime(move || {
            let st = st_rc.borrow();
            let now = Local::now();
            let (holiday_audio_opt, tts_opt, _) =
                resolve_chime_playback_params(&st.config, &st.scheduler, now);
            audio_clone.play_chime(
                1,
                st.live_volume,
                st.config.chime_play_sound,
                holiday_audio_opt,
                tts_opt,
                st.config.tts_engine,
                st.config.edge_tts_voice.clone(),
                st.config.tts_rate,
            );
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        main_win.on_sync_ntp(move || {
            if let (Some(mw), Some(sw)) = (mw_weak.upgrade(), sw_weak.upgrade()) {
                trigger_ntp_sync(&st_rc, &mw, &sw, None);
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        main_win.on_open_settings(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let st = st_rc.borrow();
                populate_settings_form(&sw, &st.config, &st.scheduler);
                center_and_show_window(&sw, 560.0, 660.0);
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        main_win.on_cycle_theme(move || {
            let mut st = st_rc.borrow_mut();
            let next_idx = (st.config.theme.to_index() + 1) % 3;
            st.config.theme = ThemeMode::from_index(next_idx);
            let _ = st.config_store.save(&st.config);
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, next_idx);
            }
        });
    }
    {
        let ctx_clone = update_ctx.clone();
        let uw_weak = update_win.as_weak();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        main_win.on_download_update(move || {
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &audio_clone,
                    &guard_rc,
                );
            }
        });
    }
    {
        let mw_weak = main_win.as_weak();
        main_win.on_dismiss_update(move || {
            if let Some(mw) = mw_weak.upgrade() {
                mw.set_has_update(false);
            }
        });
    }

    // 10. 绑定设置窗口回调（实时音量滑块 / 实时主题预览 / 手动查更新 / 校验保存 / 取消回滚）
    let last_voice_preview_gen = Rc::new(Cell::new(0u64));
    {
        let st_rc = Rc::clone(&state);
        let audio_clone = audio.clone();
        settings_win.on_preview_volume_changed(move |vol| {
            let clamped = vol.clamp(0, 100) as u8;
            st_rc.borrow_mut().live_volume = clamped;
            audio_clone.set_live_volume(clamped);
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let audio_clone = audio.clone();
        settings_win.on_test_chime(move || {
            let st = st_rc.borrow();
            let vol = st.live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let mut preview_cfg = st.config.clone();
                preview_cfg.chime_play_sound = sw.get_chime_play_sound();
                preview_cfg.chime_tts_enabled = sw.get_chime_tts_enabled();
                preview_cfg.chime_tts_template = sw.get_chime_tts_template().to_string();
                preview_cfg.holiday_enabled = sw.get_holiday_enabled();
                preview_cfg.holiday_custom_audio_path =
                    sw.get_holiday_custom_audio_path().to_string();
                preview_cfg.holiday_greeting_template =
                    sw.get_holiday_greeting_template().to_string();
                preview_cfg.holiday_custom_rules = sw.get_holiday_custom_rules().to_string();
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                let (holiday_audio_opt, tts_opt, _) =
                    resolve_chime_playback_params(&preview_cfg, &st.scheduler, Local::now());
                let _ = audio_clone.play_chime(
                    1,
                    vol,
                    preview_cfg.chime_play_sound,
                    holiday_audio_opt,
                    tts_opt,
                    tts_engine,
                    edge_voice,
                    tts_rate,
                );
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let audio_clone = audio.clone();
        let preview_gen_rc = Rc::clone(&last_voice_preview_gen);
        settings_win.on_test_voice_preview(move |include_chime| {
            let vol = st_rc.borrow().live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let now = Local::now();
                let play_sound = include_chime && sw.get_chime_play_sound();
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                let time_phrase = render_tts_template("现在时间，{TIME}", now.hour(), now.minute());
                let preview_text = format!("{time_phrase}，当前语音音色与播报音量试听正常");
                sw.set_tts_preview_status("正在准备试听语音...".into());
                let gen_id = audio_clone.play_chime(
                    1,
                    vol,
                    play_sound,
                    None,
                    Some(preview_text),
                    tts_engine,
                    edge_voice,
                    tts_rate,
                );
                preview_gen_rc.set(gen_id);
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let audio_clone = audio.clone();
        let preview_gen_rc = Rc::clone(&last_voice_preview_gen);
        settings_win.on_voice_option_switched(move |auto_preview| {
            let was_voice_previewing = audio_clone.is_playing_gen(preview_gen_rc.get());
            audio_clone.stop_active();
            preview_gen_rc.set(0);

            if auto_preview || was_voice_previewing {
                let vol = st_rc.borrow().live_volume;
                if let Some(sw) = sw_weak.upgrade() {
                    let now = Local::now();
                    let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                    let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                    let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                    let time_phrase =
                        render_tts_template("现在时间，{TIME}", now.hour(), now.minute());
                    let preview_text = format!("{time_phrase}，当前语音音色与播报音量试听正常");
                    sw.set_tts_preview_status("正在切换并试听新音色...".into());
                    let gen_id = audio_clone.play_chime(
                        1,
                        vol,
                        false,
                        None,
                        Some(preview_text),
                        tts_engine,
                        edge_voice,
                        tts_rate,
                    );
                    preview_gen_rc.set(gen_id);
                }
            } else if let Some(sw) = sw_weak.upgrade() {
                sw.set_tts_preview_status("".into());
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let audio_clone = audio.clone();
        settings_win.on_test_holiday_chime(move || {
            let st = st_rc.borrow();
            let vol = st.live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let now = Local::now();
                let mut preview_cfg = st.config.clone();
                preview_cfg.holiday_enabled = true;
                preview_cfg.holiday_custom_audio_path =
                    sw.get_holiday_custom_audio_path().to_string();
                preview_cfg.holiday_greeting_template =
                    sw.get_holiday_greeting_template().to_string();
                preview_cfg.holiday_custom_rules = sw.get_holiday_custom_rules().to_string();
                let play_sound = sw.get_chime_play_sound();
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;

                let today_holiday = st
                    .scheduler
                    .lookup_holiday(now.date_naive(), &preview_cfg)
                    .unwrap_or_else(|| HolidayInfo {
                        name: "春节".to_string(),
                        kind: HolidayKind::Statutory,
                        greeting: "新春大吉，祝各位玩家新年上分顺利、万事如意！".to_string(),
                        custom_audio_path: None,
                    });
                let custom_audio = today_holiday.custom_audio_path.clone().or_else(|| {
                    let p = preview_cfg.holiday_custom_audio_path.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                });
                let lunar_label = solar_to_lunar(now.date_naive())
                    .map(|l| l.format_zh())
                    .unwrap_or_default();
                let rendered_tts = render_holiday_template(
                    &preview_cfg.holiday_greeting_template,
                    &today_holiday.name,
                    &today_holiday.greeting,
                    &lunar_label,
                    now.hour(),
                    now.minute(),
                );
                let tts_opt = if rendered_tts.trim().is_empty() {
                    None
                } else {
                    Some(rendered_tts)
                };
                audio_clone.play_chime(
                    1,
                    vol,
                    play_sound || custom_audio.is_some(),
                    custom_audio,
                    tts_opt,
                    tts_engine,
                    edge_voice,
                    tts_rate,
                );
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        settings_win.on_browse_holiday_audio(move || {
            if let Some(path) = win_sys::browse_audio_file() {
                if let Some(sw) = sw_weak.upgrade() {
                    sw.set_holiday_custom_audio_path(path.into());
                }
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let h_tx = holiday_tx.clone();
        settings_win.on_refresh_holiday_feed(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let source = sw.get_holiday_data_source_url().to_string();
                spawn_holiday_feed_refresh(source, sw.as_weak(), h_tx.clone(), true);
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let audio_clone = audio.clone();
        settings_win.on_test_meal_chime(move || {
            let vol = st_rc.borrow().live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let custom = sw.get_meal_custom_audio_path().to_string();
                let chime_with_custom = sw.get_meal_play_chime_before_custom();
                let tts_enabled = sw.get_meal_tts_enabled();
                let tts_text = sw.get_meal_tts_text().to_string();
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                let custom_opt = if custom.trim().is_empty() {
                    None
                } else {
                    Some(custom)
                };
                let tts_opt = build_meal_tts_text(tts_enabled, &tts_text, Local::now());
                audio_clone.play_meal(
                    1,
                    vol,
                    custom_opt,
                    chime_with_custom,
                    tts_opt,
                    tts_engine,
                    edge_voice,
                    tts_rate,
                );
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        settings_win.on_browse_meal_audio(move || {
            if let Some(path) = win_sys::browse_audio_file() {
                if let Some(sw) = sw_weak.upgrade() {
                    sw.set_meal_custom_audio_path(path.into());
                }
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        settings_win.on_sync_ntp(move || {
            if let (Some(mw), Some(sw)) = (mw_weak.upgrade(), sw_weak.upgrade()) {
                let input_server = sw.get_ntp_server().to_string();
                let trimmed = input_server.trim().to_string();
                let target = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
                trigger_ntp_sync(&st_rc, &mw, &sw, target);
            }
        });
    }
    {
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        settings_win.on_select_theme(move |idx| {
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, idx);
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let wc_rc = Rc::clone(&web_console);
        settings_win.on_web_console_toggled(move |enabled| {
            if let Some(sw) = sw_weak.upgrade() {
                if enabled {
                    match wc_rc.borrow_mut().start(DEFAULT_WEB_CONSOLE_PORT) {
                        Ok(url) => {
                            sw.set_web_console_url(url.clone().into());
                            sw.set_web_console_status(
                                format!("已开启本地服务：可点击右侧“打开网页”访问 {url}").into(),
                            );
                        }
                        Err(err) => {
                            sw.set_web_console_enabled(false);
                            sw.set_web_console_status(err.into());
                        }
                    }
                } else {
                    wc_rc.borrow_mut().stop();
                    sw.set_web_console_status("已关闭：本地 3000 端口已停止监听".into());
                }
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        settings_win.on_open_web_console(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let url = sw.get_web_console_url().to_string();
                if let Err(err) = win_sys::open_url_in_browser(&url) {
                    sw.set_web_console_status(err.into());
                }
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let mw_weak = main_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let audio_clone = audio.clone();
        let wc_rc = Rc::clone(&web_console);
        settings_win.on_cancel_clicked(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            st.live_volume = st.config.volume;
            audio_clone.set_live_volume(st.config.volume);
            if st.config.web_console_enabled {
                let _ = wc_rc.borrow_mut().start(DEFAULT_WEB_CONSOLE_PORT);
            } else {
                wc_rc.borrow_mut().stop();
            }
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, st.config.theme.to_index());
                let _ = sw.hide();
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let mw_weak = main_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let tray_clone = Arc::clone(&tray);
        let audio_clone = audio.clone();
        let u_tx = update_tx.clone();
        let h_tx = holiday_tx.clone();
        let wc_rc = Rc::clone(&web_console);
        settings_win.on_save_clicked(move || {
            let (Some(sw), Some(mw), Some(aw), Some(uw)) = (
                sw_weak.upgrade(),
                mw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) else {
                return;
            };

            let raw_minutes = sw.get_chime_minutes_str().to_string();
            let parsed_minutes = match AppConfig::parse_chime_minutes(&raw_minutes) {
                Ok(v) => v,
                Err(err) => {
                    sw.set_validation_error(err.into());
                    return;
                }
            };

            let (
                preserved_mute_mode,
                prev_holiday_url,
                preserved_update_url,
                preserved_update_token,
                preserved_update_interval,
            ) = {
                let st = st_rc.borrow();
                let u_url = if st.config.update_check_url.trim().is_empty() {
                    version::DEFAULT_GITHUB_RELEASE_URL.to_string()
                } else {
                    st.config.update_check_url.clone()
                };
                (
                    st.config.mute_mode,
                    st.config.holiday_data_source_url.clone(),
                    u_url,
                    st.config.update_github_token.clone(),
                    st.config.update_check_interval_hours.clamp(1, 168),
                )
            };
            let mut candidate = AppConfig {
                chime_minutes: parsed_minutes,
                chime_repeat_count: sw.get_chime_repeat().clamp(1, 10) as u8,
                chime_play_sound: sw.get_chime_play_sound(),
                chime_tts_enabled: sw.get_chime_tts_enabled(),
                chime_tts_template: sw.get_chime_tts_template().to_string(),
                holiday_enabled: sw.get_holiday_enabled(),
                holiday_custom_audio_path: sw.get_holiday_custom_audio_path().to_string(),
                holiday_greeting_template: sw.get_holiday_greeting_template().to_string(),
                holiday_custom_rules: sw.get_holiday_custom_rules().to_string(),
                holiday_data_source_url: sw.get_holiday_data_source_url().to_string(),
                mute_mode: preserved_mute_mode,
                quiet_enabled: sw.get_quiet_enabled(),
                quiet_start_hour: sw.get_quiet_start_hour().clamp(0, 23) as u8,
                quiet_start_minute: sw.get_quiet_start_minute().clamp(0, 59) as u8,
                quiet_end_hour: sw.get_quiet_end_hour().clamp(0, 23) as u8,
                quiet_end_minute: sw.get_quiet_end_minute().clamp(0, 59) as u8,
                meal_enabled: sw.get_meal_enabled(),
                meal_hour: sw.get_meal_hour().clamp(0, 23) as u8,
                meal_minute: sw.get_meal_minute().clamp(0, 59) as u8,
                meal_repeat_count: sw.get_meal_repeat().clamp(1, 10) as u8,
                meal_custom_audio_path: sw.get_meal_custom_audio_path().to_string(),
                meal_play_chime_before_custom: sw.get_meal_play_chime_before_custom(),
                meal_tts_enabled: sw.get_meal_tts_enabled(),
                meal_tts_text: sw.get_meal_tts_text().to_string(),
                tts_engine: TtsEngine::from_index(sw.get_tts_engine_index()),
                edge_tts_voice: edge_voice_from_index(sw.get_edge_voice_index()).to_string(),
                tts_rate: sw.get_tts_rate().clamp(-5, 5) as i8,
                volume: sw.get_volume().clamp(0, 100) as u8,
                ntp_server: sw.get_ntp_server().to_string(),
                auto_start: sw.get_auto_start(),
                theme: ThemeMode::from_index(sw.get_theme_index()),
                update_check_enabled: true,
                update_check_url: preserved_update_url,
                update_check_interval_hours: preserved_update_interval,
                update_github_token: preserved_update_token,
                web_console_enabled: sw.get_web_console_enabled(),
            };

            if let Err(err) = candidate.validate() {
                sw.set_validation_error(err.into());
                return;
            }

            if candidate.web_console_enabled {
                if let Err(err) = wc_rc.borrow_mut().start(DEFAULT_WEB_CONSOLE_PORT) {
                    sw.set_validation_error(err.into());
                    return;
                }
            } else {
                wc_rc.borrow_mut().stop();
            }

            if let Err(err) = win_sys::apply_auto_start(candidate.auto_start) {
                sw.set_validation_error(err.into());
                return;
            }

            {
                let mut st = st_rc.borrow_mut();
                if let Err(err) = st.config_store.save(&candidate) {
                    sw.set_validation_error(err.into());
                    return;
                }
                st.config = candidate.clone();
                st.live_volume = candidate.volume;
                st.scheduler.reset_slots();
                refresh_main_window_ui(&mw, &st);
            }
            wc_rc
                .borrow()
                .update_config_view(config_to_web_view(&candidate));

            if candidate.holiday_data_source_url != prev_holiday_url {
                spawn_holiday_feed_refresh(
                    candidate.holiday_data_source_url.clone(),
                    sw.as_weak(),
                    h_tx.clone(),
                    false,
                );
            }

            let _ = u_tx.send(UpdateWorkerMsg::Reconfigure {
                enabled: candidate.update_check_enabled,
                url: candidate.update_check_url.clone(),
                token: candidate.update_github_token.clone(),
                interval_hours: candidate.update_check_interval_hours,
            });

            tray_clone.set_auto_start_checked(candidate.auto_start);
            audio_clone.set_live_volume(candidate.volume);
            sync_theme_across_windows(&mw, &sw, &aw, &uw, candidate.theme.to_index());
            sw.set_validation_error("".into());
            let _ = sw.hide();
        });
    }

    // 11. 关于弹窗与更新提示弹窗回调
    {
        let aw_weak = about_win.as_weak();
        about_win.on_close_clicked(move || {
            if let Some(aw) = aw_weak.upgrade() {
                let _ = aw.hide();
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let aw_weak = about_win.as_weak();
        let u_tx = update_tx.clone();
        about_win.on_check_update_now(move || {
            if let Some(aw) = aw_weak.upgrade() {
                let st = st_rc.borrow();
                trigger_manual_update_check(
                    &u_tx,
                    &aw,
                    st.config.update_check_url.clone(),
                    st.config.update_github_token.clone(),
                );
            }
        });
    }
    {
        let ctx_clone = update_ctx.clone();
        let uw_weak = update_win.as_weak();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        about_win.on_download_update(move || {
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &audio_clone,
                    &guard_rc,
                );
            }
        });
    }
    {
        let ctx_clone = update_ctx.clone();
        let uw_weak = update_win.as_weak();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        update_win.on_auto_update_clicked(move || {
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &audio_clone,
                    &guard_rc,
                );
            }
        });
    }
    {
        let ctx_clone = update_ctx.clone();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        update_win.on_browser_download_clicked(move || {
            if let Some(aw) = aw_weak.upgrade() {
                open_downloaded_update(&ctx_clone, &aw);
            }
            if let Some(uw) = uw_weak.upgrade() {
                let _ = uw.hide();
            }
        });
    }
    {
        let uw_weak = update_win.as_weak();
        update_win.on_later_clicked(move || {
            if let Some(uw) = uw_weak.upgrade() {
                let _ = uw.hide();
            }
        });
    }

    // 12. 绑定系统托盘双击与右键菜单事件（FR-15 / FR-16 / FR-17）
    let tray_open_pending = Arc::new(AtomicBool::new(false));
    {
        let open_flag = Arc::clone(&tray_open_pending);
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                open_flag.store(true, Ordering::SeqCst);
            }
        }));
    }
    {
        let tray_clone = Arc::clone(&tray);
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        let aw_weak = about_win.as_weak();
        let st_rc_main_thread = Rc::clone(&state);
        let audio_clone = audio.clone();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let tray_open_flag = Arc::clone(&tray_open_pending);

        // 通过 Slint 事件循环安全调度托盘菜单与双击指令
        let (menu_tx, menu_rx) = std::sync::mpsc::channel::<tray_icon::menu::MenuId>();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = menu_tx.send(event.id);
        }));

        let menu_timer = Timer::default();
        menu_timer.start(TimerMode::Repeated, Duration::from_millis(120), move || {
            if tray_open_flag.swap(false, Ordering::SeqCst) {
                if let Some(mw) = mw_weak.upgrade() {
                    refresh_main_window_ui(&mw, &st_rc_main_thread.borrow());
                    center_and_show_window(&mw, 460.0, 376.0);
                }
            }
            while let Ok(id) = menu_rx.try_recv() {
                if id == tray_clone.ids.test_chime {
                    let st = st_rc_main_thread.borrow();
                    let now = Local::now();
                    let (holiday_audio_opt, tts_opt, _) =
                        resolve_chime_playback_params(&st.config, &st.scheduler, now);
                    audio_clone.play_chime(
                        1,
                        st.live_volume,
                        st.config.chime_play_sound,
                        holiday_audio_opt,
                        tts_opt,
                        st.config.tts_engine,
                        st.config.edge_tts_voice.clone(),
                        st.config.tts_rate,
                    );
                } else if id == tray_clone.ids.sync_ntp {
                    if let (Some(mw), Some(sw)) = (mw_weak.upgrade(), sw_weak.upgrade()) {
                        trigger_ntp_sync(&st_rc_main_thread, &mw, &sw, None);
                    }
                } else if id == tray_clone.ids.mute_mode {
                    let mut st = st_rc_main_thread.borrow_mut();
                    let next_muted = !st.config.mute_mode;
                    st.config.mute_mode = next_muted;
                    let _ = st.config_store.save(&st.config);
                    tray_clone.set_mute_mode_checked(next_muted);
                    if next_muted {
                        audio_clone.stop_active();
                    }
                    win_sys::log_info("tray", &format!("toggle mute_mode={next_muted}"));
                    if let Some(mw) = mw_weak.upgrade() {
                        refresh_main_window_ui(&mw, &st);
                    }
                } else if id == tray_clone.ids.open_settings {
                    if let Some(sw) = sw_weak.upgrade() {
                        let st = st_rc_main_thread.borrow();
                        populate_settings_form(&sw, &st.config, &st.scheduler);
                        center_and_show_window(&sw, 560.0, 660.0);
                    }
                } else if id == tray_clone.ids.auto_start {
                    let mut st = st_rc_main_thread.borrow_mut();
                    let next_val = !st.config.auto_start;
                    if win_sys::apply_auto_start(next_val).is_ok() {
                        st.config.auto_start = next_val;
                        let _ = st.config_store.save(&st.config);
                        tray_clone.set_auto_start_checked(next_val);
                        if let Some(sw) = sw_weak.upgrade() {
                            sw.set_auto_start(next_val);
                        }
                    }
                } else if id == tray_clone.ids.open_logs_folder {
                    let _ = win_sys::open_logs_folder();
                } else if id == tray_clone.ids.check_update {
                    if let Some(aw) = aw_weak.upgrade() {
                        let st = st_rc_main_thread.borrow();
                        trigger_manual_update_check(
                            &u_tx,
                            &aw,
                            st.config.update_check_url.clone(),
                            st.config.update_github_token.clone(),
                        );
                        center_and_show_window(&aw, 452.0, 372.0);
                    }
                } else if id == tray_clone.ids.about {
                    if let Some(aw) = aw_weak.upgrade() {
                        center_and_show_window(&aw, 452.0, 372.0);
                    }
                } else if id == tray_clone.ids.exit {
                    let _ = u_tx.send(UpdateWorkerMsg::Shutdown);
                    let _ = perf_tx.send(());
                    audio_clone.shutdown();
                    let _ = slint::quit_event_loop();
                }
            }
        });
        std::mem::forget(menu_timer);
    }

    // 13. 核心 250ms 节拍调度定时器（FR-01..FR-08 / FR-18..FR-20）
    let tick_timer = Timer::default();
    {
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let sw_weak = settings_win.as_weak();
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let audio_clone = audio.clone();
        let tray_for_tick = Arc::clone(&tray);
        let ipc_wake_for_tick = Arc::clone(&ipc_wake_pending);
        let preview_gen_tick = Rc::clone(&last_voice_preview_gen);
        let wc_tick = Rc::clone(&web_console);
        let perf_sample_tick = Arc::clone(&latest_perf);
        let update_ctx_tick = update_ctx.clone();
        let u_tx_tick = update_tx.clone();
        let h_tx_tick = holiday_tx.clone();
        let perf_stop_tick = perf_stop_tx.clone();
        let guard_tick = Rc::clone(&single_instance_guard);

        tick_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let now = Local::now();

            // 处理本地 Web 控制台触发的控制指令
            while let Ok(action) = web_action_rx.try_recv() {
                match action {
                    WebAction::TestChime => {
                        let st = st_rc.borrow();
                        let (holiday_audio_opt, tts_opt, _) =
                            resolve_chime_playback_params(&st.config, &st.scheduler, now);
                        audio_clone.play_chime(
                            1,
                            st.live_volume,
                            st.config.chime_play_sound,
                            holiday_audio_opt,
                            tts_opt,
                            st.config.tts_engine,
                            st.config.edge_tts_voice.clone(),
                            st.config.tts_rate,
                        );
                    }
                    WebAction::TestVoice {
                        tts_engine_index,
                        edge_voice_index,
                        tts_rate,
                    } => {
                        let was_playing = audio_clone.is_playing_gen(preview_gen_tick.get());
                        let _ = was_playing;
                        audio_clone.stop_active();
                        let mut st = st_rc.borrow_mut();
                        let engine = tts_engine_index
                            .map(TtsEngine::from_index)
                            .unwrap_or(st.config.tts_engine);
                        let voice = edge_voice_index
                            .map(|idx| edge_voice_from_index(idx).to_string())
                            .unwrap_or_else(|| st.config.edge_tts_voice.clone());
                        let rate = tts_rate
                            .map(|r| r.clamp(-5, 5))
                            .unwrap_or(st.config.tts_rate);
                        let time_phrase =
                            render_tts_template("现在时间，{TIME}", now.hour(), now.minute());
                        let preview_text =
                            format!("{time_phrase}，当前语音音色与播报音量试听正常");
                        st.tts_preview_status = "正在准备试听语音...".to_string();
                        if let Some(sw) = sw_weak.upgrade() {
                            sw.set_tts_preview_status("正在准备试听语音...".into());
                        }
                        let gen_id = audio_clone.play_chime(
                            1,
                            st.live_volume,
                            false,
                            None,
                            Some(preview_text),
                            engine,
                            voice,
                            rate,
                        );
                        preview_gen_tick.set(gen_id);
                    }
                    WebAction::TestMeal => {
                        let st = st_rc.borrow();
                        let custom_opt = if st.config.meal_custom_audio_path.trim().is_empty() {
                            None
                        } else {
                            Some(st.config.meal_custom_audio_path.clone())
                        };
                        let tts_opt = build_meal_tts_text(
                            st.config.meal_tts_enabled,
                            &st.config.meal_tts_text,
                            now,
                        );
                        audio_clone.play_meal(
                            1,
                            st.live_volume,
                            custom_opt,
                            st.config.meal_play_chime_before_custom,
                            tts_opt,
                            st.config.tts_engine,
                            st.config.edge_tts_voice.clone(),
                            st.config.tts_rate,
                        );
                    }
                    WebAction::ToggleMute => {
                        let mut st = st_rc.borrow_mut();
                        let next_muted = !st.config.mute_mode;
                        st.config.mute_mode = next_muted;
                        let _ = st.config_store.save(&st.config);
                        tray_for_tick.set_mute_mode_checked(next_muted);
                        if next_muted {
                            audio_clone.stop_active();
                        }
                        if let Some(mw) = mw_weak.upgrade() {
                            refresh_main_window_ui(&mw, &st);
                        }
                    }
                    WebAction::SyncNtp { server } => {
                        if let (Some(mw), Some(sw)) = (mw_weak.upgrade(), sw_weak.upgrade()) {
                            let target = server.filter(|s| !s.trim().is_empty());
                            trigger_ntp_sync(&st_rc, &mw, &sw, target);
                        }
                    }
                    WebAction::SetVolume(vol) => {
                        let clamped = vol.min(100);
                        let mut st = st_rc.borrow_mut();
                        st.live_volume = clamped;
                        st.config.volume = clamped;
                        let _ = st.config_store.save(&st.config);
                        audio_clone.set_live_volume(clamped);
                        if let Some(sw) = sw_weak.upgrade() {
                            sw.set_volume(clamped as i32);
                        }
                        wc_tick.borrow().update_config_view(config_to_web_view(&st.config));
                    }
                    WebAction::SetTheme(idx) => {
                        let mut st = st_rc.borrow_mut();
                        st.config.theme = ThemeMode::from_index(idx);
                        let norm_idx = st.config.theme.to_index();
                        let _ = st.config_store.save(&st.config);
                        if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                            mw_weak.upgrade(),
                            sw_weak.upgrade(),
                            aw_weak.upgrade(),
                            uw_weak.upgrade(),
                        ) {
                            sync_theme_across_windows(&mw, &sw, &aw, &uw, norm_idx);
                            sw.set_theme_index(norm_idx);
                        }
                        wc_tick.borrow().update_config_view(config_to_web_view(&st.config));
                    }
                    WebAction::TrimMemory(reply_tx) => {
                        let msg = perf_guard::trim_memory_now(&audio_clone);
                        let _ = reply_tx.send(Ok(msg));
                    }
                    WebAction::RefreshHolidays(reply_tx) => {
                        let source = st_rc.borrow().config.holiday_data_source_url.clone();
                        if let Some(sw) = sw_weak.upgrade() {
                            spawn_holiday_feed_refresh(source, sw.as_weak(), h_tx_tick.clone(), true);
                        }
                        let _ = reply_tx.send(Ok("已发起节假日数据源刷新请求".to_string()));
                    }
                    WebAction::CheckUpdate => {
                        if let Some(aw) = aw_weak.upgrade() {
                            let st = st_rc.borrow();
                            trigger_manual_update_check(
                                &u_tx_tick,
                                &aw,
                                st.config.update_check_url.clone(),
                                st.config.update_github_token.clone(),
                            );
                        }
                    }
                    WebAction::ApplyUpdate => {
                        if let Some(uw) = uw_weak.upgrade() {
                            download_and_apply_auto_update(
                                &update_ctx_tick,
                                &uw,
                                &u_tx_tick,
                                &perf_stop_tick,
                                &audio_clone,
                                &guard_tick,
                            );
                        }
                    }
                    WebAction::OpenLogs => {
                        let _ = win_sys::open_logs_folder();
                    }
                    WebAction::SaveConfig(patch, reply_tx) => {
                        let res = (|| -> Result<String, String> {
                            let prev_holiday_url = st_rc.borrow().config.holiday_data_source_url.clone();
                            let prev_auto_start = st_rc.borrow().config.auto_start;
                            let candidate = {
                                let st = st_rc.borrow();
                                apply_web_config_patch(&st.config, patch)?
                            };
                            if candidate.auto_start != prev_auto_start {
                                win_sys::apply_auto_start(candidate.auto_start)?;
                            }
                            {
                                let mut st = st_rc.borrow_mut();
                                st.config_store.save(&candidate)?;
                                st.config = candidate.clone();
                                st.live_volume = candidate.volume;
                                st.scheduler.reset_slots();
                                if let Some(mw) = mw_weak.upgrade() {
                                    refresh_main_window_ui(&mw, &st);
                                }
                                if let Some(sw) = sw_weak.upgrade() {
                                    populate_settings_form(&sw, &st.config, &st.scheduler);
                                }
                            }
                            if let Ok(mut tok_g) = update_ctx_tick.github_token.lock() {
                                *tok_g = candidate.update_github_token.clone();
                            }
                            let _ = u_tx_tick.send(UpdateWorkerMsg::Reconfigure {
                                enabled: candidate.update_check_enabled,
                                url: candidate.update_check_url.clone(),
                                token: candidate.update_github_token.clone(),
                                interval_hours: candidate.update_check_interval_hours,
                            });
                            if candidate.holiday_data_source_url != prev_holiday_url {
                                if let Some(sw) = sw_weak.upgrade() {
                                    spawn_holiday_feed_refresh(
                                        candidate.holiday_data_source_url.clone(),
                                        sw.as_weak(),
                                        h_tx_tick.clone(),
                                        false,
                                    );
                                }
                            }
                            tray_for_tick.set_auto_start_checked(candidate.auto_start);
                            audio_clone.set_live_volume(candidate.volume);
                            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                                mw_weak.upgrade(),
                                sw_weak.upgrade(),
                                aw_weak.upgrade(),
                                uw_weak.upgrade(),
                            ) {
                                sync_theme_across_windows(
                                    &mw,
                                    &sw,
                                    &aw,
                                    &uw,
                                    candidate.theme.to_index(),
                                );
                            }
                            wc_tick.borrow().update_config_view(config_to_web_view(&candidate));
                            Ok("配置已保存并立即生效".to_string())
                        })();
                        let _ = reply_tx.send(res);
                    }
                }
            }

            let mut st = st_rc.borrow_mut();

            // 同步语音试听状态至设置窗口提示行与本地控制台快照
            while let Ok((gen_id, status_msg)) = tts_status_rx.try_recv() {
                if gen_id != 0 && gen_id == preview_gen_tick.get() {
                    st.tts_preview_status = status_msg.clone();
                    if let Some(sw) = sw_weak.upgrade() {
                        sw.set_tts_preview_status(status_msg.into());
                    }
                }
            }

            // 接收异步拉取完成的外部节假日列表并刷新历法预览
            while let Ok((res, _manual)) = holiday_rx.try_recv() {
                match res {
                    Ok(entries) => {
                        let count = entries.len();
                        st.scheduler.set_external_holidays(entries);
                        st.holiday_feed_status = if count > 0 {
                            format!("已同步 {count} 条外部节假日规则")
                        } else {
                            "使用内置公历/农历节日表".to_string()
                        };
                        if let Some(sw) = sw_weak.upgrade() {
                            sw.set_holiday_today_preview(
                                st.scheduler
                                    .describe_day_calendar(now.date_naive(), &st.config)
                                    .into(),
                            );
                        }
                    }
                    Err(err) => {
                        st.holiday_feed_status = format!("同步失败（仍保留内置农历库）：{err}");
                    }
                }
            }

            // 同步后台异步校时写回到 UI 的完成状态
            if let Some(mw) = mw_weak.upgrade() {
                if st.is_syncing && !mw.get_is_syncing() {
                    st.is_syncing = false;
                    st.ntp_status_code = mw.get_ntp_status_code();
                    st.ntp_status_text = mw.get_ntp_status_text().to_string();
                    st.scheduler.reset_slots();
                }
            }

            let config_snapshot = st.config.clone();
            let volume_snapshot = st.live_volume;
            let events = st.scheduler.evaluate_at(&config_snapshot, now);

            for ev in events {
                match ev {
                    TriggerEvent::Chime {
                        repeat,
                        spoken_time_text,
                        holiday_audio_path,
                        holiday_name,
                    } => {
                        win_sys::log_info(
                            "scheduler",
                            &format!(
                                "trigger chime repeat={repeat} volume={volume_snapshot} sound={} holiday={:?} holiday_audio={:?} tts={:?}",
                                config_snapshot.chime_play_sound,
                                holiday_name,
                                holiday_audio_path,
                                spoken_time_text
                            ),
                        );
                        audio_clone.play_chime(
                            repeat,
                            volume_snapshot,
                            config_snapshot.chime_play_sound,
                            holiday_audio_path,
                            spoken_time_text,
                            config_snapshot.tts_engine,
                            config_snapshot.edge_tts_voice.clone(),
                            config_snapshot.tts_rate,
                        );
                    }
                    TriggerEvent::Meal {
                        repeat,
                        spoken_text,
                    } => {
                        let custom_opt = if config_snapshot.meal_custom_audio_path.trim().is_empty()
                        {
                            None
                        } else {
                            Some(config_snapshot.meal_custom_audio_path.clone())
                        };
                        win_sys::log_info(
                            "scheduler",
                            &format!(
                                "trigger meal repeat={repeat} volume={volume_snapshot} custom={:?} chime_before={} tts={:?}",
                                custom_opt, config_snapshot.meal_play_chime_before_custom, spoken_text
                            ),
                        );
                        audio_clone.play_meal(
                            repeat,
                            volume_snapshot,
                            custom_opt,
                            config_snapshot.meal_play_chime_before_custom,
                            spoken_text,
                            config_snapshot.tts_engine,
                            config_snapshot.edge_tts_voice.clone(),
                            config_snapshot.tts_rate,
                        );
                    }
                }
            }

            let wake_now = ipc_wake_for_tick.swap(false, Ordering::SeqCst);
            if let Some(mw) = mw_weak.upgrade() {
                if wake_now {
                    refresh_main_window_ui(&mw, &st);
                    center_and_show_window(&mw, 460.0, 376.0);
                } else if mw.window().is_visible() {
                    // 仅当主状态窗口可见时刷新界面文本，最小化常驻托盘期间跳过字符串分配，保持零堆内存增长
                    refresh_main_window_ui(&mw, &st);
                }
            }

            if wc_tick.borrow().is_running() {
                let next = Scheduler::compute_next_event(&st.config, now);
                let is_quiet_now = Scheduler::is_quiet_at(
                    &st.config,
                    chrono::Timelike::hour(&now),
                    chrono::Timelike::minute(&now),
                );
                let quiet_summary = if st.config.quiet_enabled {
                    format!(
                        "{:02}:{:02}-{:02}:{:02}",
                        st.config.quiet_start_hour,
                        st.config.quiet_start_minute,
                        st.config.quiet_end_hour,
                        st.config.quiet_end_minute
                    )
                } else {
                    "未启用静默时段".to_string()
                };
                let (fallback_ws_mb, fallback_priv_mb) = perf_guard::query_memory_mb_now();
                let perf_snap = perf_sample_tick
                    .lock()
                    .map(|g| *g)
                    .unwrap_or_default();
                let ws_mb = if perf_snap.working_set_bytes > 0 {
                    perf_guard::format_mb(perf_snap.working_set_bytes)
                } else {
                    fallback_ws_mb
                };
                let priv_mb = if perf_snap.private_bytes > 0 {
                    perf_guard::format_mb(perf_snap.private_bytes)
                } else {
                    fallback_priv_mb
                };

                wc_tick.borrow().update_snapshot(WebSnapshot {
                    version: version::display_version(),
                    current_time: now.format("%H:%M:%S").to_string(),
                    current_date: format!("{} · {}", now.format("%Y-%m-%d"), weekday_zh(now.weekday())),
                    mute_mode: st.config.mute_mode,
                    is_quiet_now,
                    quiet_summary,
                    next_event_title: next.title,
                    next_event_time: next.target_time_text,
                    next_event_countdown: next.countdown_text,
                    chime_minutes: st.config.format_chime_minutes(),
                    tts_engine: st.config.tts_engine.display_name().to_string(),
                    tts_engine_index: st.config.tts_engine.to_index(),
                    edge_tts_voice: st.config.edge_tts_voice.clone(),
                    edge_voice_index: edge_voice_to_index(&st.config.edge_tts_voice),
                    tts_rate: st.config.tts_rate,
                    tts_preview_status: st.tts_preview_status.clone(),
                    volume: st.live_volume,
                    theme_index: st.config.theme.to_index(),
                    ntp_status: st.ntp_status_text.clone(),
                    is_syncing: st.is_syncing,
                    memory_ws_mb: ws_mb,
                    memory_private_mb: priv_mb,
                    process_cpu_pct: perf_snap.process_cpu_pct,
                    system_cpu_pct: perf_snap.system_cpu_pct,
                    uptime_text: format_uptime(app_started_at.elapsed()),
                    config_path: st.config_store.path.to_string_lossy().to_string(),
                    log_path: win_sys::current_log_path()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    today_calendar_preview: st
                        .scheduler
                        .describe_day_calendar(now.date_naive(), &st.config),
                    holiday_feed_status: st.holiday_feed_status.clone(),
                    update_checking: update_ctx_tick.is_checking.load(Ordering::SeqCst),
                    has_update: update_ctx_tick.has_update.load(Ordering::SeqCst),
                    update_version: update_ctx_tick
                        .remote_version
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_default(),
                    update_status_text: update_ctx_tick
                        .status_text
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_default(),
                    update_notes: update_ctx_tick
                        .release_notes
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_default(),
                    update_downloading: update_ctx_tick.is_downloading.load(Ordering::SeqCst),
                    update_progress: update_ctx_tick.download_progress.load(Ordering::SeqCst),
                    update_download_status: update_ctx_tick
                        .download_status_text
                        .lock()
                        .map(|g| g.clone())
                        .unwrap_or_default(),
                });
            }
        });
    }

    // 支持带 --show 参数启动时立即居中显示主窗口（默认隐藏常驻托盘）
    if args.iter().any(|a| a == "--show") {
        center_and_show_window(&main_win, 460.0, 376.0);
    }

    // 14. 进入事件主循环（仅通过托盘菜单“退出”调用 quit_event_loop 时结束）
    let _ = slint::run_event_loop_until_quit();
    win_sys::log_info("lifecycle", "application shutting down cleanly");
    web_console.borrow_mut().stop();
    let _ = perf_stop_tx.send(());
    let _ = update_tx.send(UpdateWorkerMsg::Shutdown);
    audio.shutdown();
    win_sys::flush_logs();
}

#[cfg(test)]
mod perf_tests {
    use super::perf_guard::*;
    use std::time::Duration;

    #[test]
    fn gc_decision_triggers_above_50mb_and_respects_cooldown() {
        let under = PerfSample {
            working_set_bytes: 22 * 1024 * 1024,
            private_bytes: 18 * 1024 * 1024,
            process_cpu_pct: 0.3,
            system_cpu_pct: 12.0,
            source: "test",
        };
        assert_eq!(
            evaluate_gc_decision(&under, None, MEMORY_GC_THRESHOLD_BYTES, GC_COOLDOWN),
            GcDecision::None
        );

        let over = PerfSample {
            working_set_bytes: 54 * 1024 * 1024,
            private_bytes: 49 * 1024 * 1024,
            process_cpu_pct: 1.5,
            system_cpu_pct: 30.0,
            source: "test",
        };
        assert_eq!(
            evaluate_gc_decision(&over, None, MEMORY_GC_THRESHOLD_BYTES, GC_COOLDOWN),
            GcDecision::MemoryExceeded
        );
        assert_eq!(
            evaluate_gc_decision(
                &over,
                Some(Duration::from_secs(45)),
                MEMORY_GC_THRESHOLD_BYTES,
                GC_COOLDOWN
            ),
            GcDecision::CooldownActive
        );
        assert_eq!(
            evaluate_gc_decision(
                &over,
                Some(Duration::from_secs(125)),
                MEMORY_GC_THRESHOLD_BYTES,
                GC_COOLDOWN
            ),
            GcDecision::MemoryExceeded
        );
    }

    #[test]
    fn cpu_overload_detects_process_and_system_spikes() {
        let normal = PerfSample {
            working_set_bytes: 18 * 1024 * 1024,
            private_bytes: 14 * 1024 * 1024,
            process_cpu_pct: 1.2,
            system_cpu_pct: 40.0,
            source: "test",
        };
        assert!(!is_cpu_overloaded(&normal));

        let high_proc = PerfSample {
            process_cpu_pct: 18.5,
            ..normal
        };
        assert!(is_cpu_overloaded(&high_proc));

        let high_sys = PerfSample {
            system_cpu_pct: 91.0,
            ..normal
        };
        assert!(is_cpu_overloaded(&high_sys));
    }
}

