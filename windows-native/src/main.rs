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
    AlarmRepeatMode, AppConfig, ConfigStore, CustomAlarm, ThemeMode, TtsEngine,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use scheduler::{
    solar_to_lunar, HolidayInfo, HolidayKind, ScheduledRefContext, Scheduler, TriggerEvent,
};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
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
    tts_preview_status: String,
    last_triggered_alarm: Option<CustomAlarm>,
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
    fallback_download_url: Arc<Mutex<String>>,
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

/// Windows 原生性能计数器监控与内存强制回收模块（保障低配设备长期驻留稳定运行）
mod perf_guard {
    use super::audio::AudioEngine;
    use super::win_sys;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// 内存常驻告警与强制回收阈值：85 MiB（适配 Slint 窗口与 Windows Web/DLL 基础提交集，避免在 50 MiB 处频繁抖动工作集）
    pub const MEMORY_GC_THRESHOLD_BYTES: u64 = 85 * 1024 * 1024;
    /// 本进程 CPU 负载偏高阈值（%）
    pub const PROCESS_CPU_HIGH_THRESHOLD_PCT: f64 = 15.0;
    /// 整机 CPU 负载偏高阈值（%，触发后台非核心任务让步）
    pub const SYSTEM_CPU_HIGH_THRESHOLD_PCT: f64 = 85.0;
    /// 后台巡检采样间隔（30 秒一次，自身开销几乎为 0）
    pub const PERF_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
    /// 强制回收冷却间隔（5 分钟，避免反复清空工作集引发硬缺页抖动）
    pub const GC_COOLDOWN: Duration = Duration::from_secs(300);

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
        let exceeded = sample.working_set_bytes > threshold_bytes;
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
        use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
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

        /// 内核态时间片兜底追踪器（兼容精简版 Windows 系统禁用 PDH 性能注册表的情况）
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

        #[inline]
        pub fn query_process_memory_bytes() -> (u64, u64) {
            win_sys::query_process_memory_bytes()
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

        #[inline]
        pub fn query_process_memory_bytes() -> (u64, u64) {
            win_sys::query_process_memory_bytes()
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

    /// 启动后台资源与性能计数器监控线程，并在超限（内存 > 85MB 且非播报期间）时执行强制内存回收
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
                            if audio.is_busy() {
                                win_sys::log_debug(
                                    "perf",
                                    &format!(
                                        "memory above 85MB (ws={:.2}MB, private={:.2}MB) but deferred while audio is busy",
                                        format_mb(sample.working_set_bytes),
                                        format_mb(sample.private_bytes)
                                    ),
                                );
                            } else {
                                let (before_ws, after_ws, compacted) =
                                    imp::force_windows_memory_reclaim(&audio);
                                last_gc_at = Some(Instant::now());
                                win_sys::log_info(
                                    "perf",
                                    &format!(
                                        "memory exceeded >85MB ({}), forced reclaim: ws {:.2}MB -> {:.2}MB (private={:.2}MB, heap_largest_free={}B, proc_cpu={:.1}%, sys_cpu={:.1}%)",
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
                        }
                        GcDecision::CooldownActive => {
                            win_sys::log_debug(
                                "perf",
                                &format!(
                                    "memory above 85MB (ws={:.2}MB, private={:.2}MB) but GC cooldown active",
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

fn build_ureq_agent_with_options(
    connect_secs: u64,
    read_secs: u64,
    redirects: u32,
    use_proxy: bool,
) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(connect_secs))
        .timeout_read(Duration::from_secs(read_secs))
        .redirects(redirects);

    if let Ok(connector) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(Arc::new(connector));
    }

    if use_proxy {
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
    }

    builder.build()
}

fn build_ureq_agent_with_redirects(connect_secs: u64, read_secs: u64, redirects: u32) -> ureq::Agent {
    build_ureq_agent_with_options(connect_secs, read_secs, redirects, true)
}

fn build_ureq_agent(connect_secs: u64, read_secs: u64) -> ureq::Agent {
    build_ureq_agent_with_options(connect_secs, read_secs, 5, true)
}

fn build_ureq_agent_direct(connect_secs: u64, read_secs: u64) -> ureq::Agent {
    build_ureq_agent_with_options(connect_secs, read_secs, 5, false)
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

fn fetch_public_gitee_release_via_redirect(
    agent: &ureq::Agent,
    owner: &str,
    repo: &str,
) -> Option<Result<version::RemoteVersionManifest, String>> {
    let web_latest = format!("https://gitee.com/{owner}/{repo}/releases/latest");
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

    let exe_url =
        format!("https://gitee.com/{owner}/{repo}/releases/download/{tag}/HalfHourChime.exe");
    Some(Ok(version::RemoteVersionManifest {
        version: tag.to_string(),
        download_url: exe_url,
        api_download_url: String::new(),
        notes: "已通过 Gitee 国内直连源检测到新版本，点击“自动更新”即可下载升级。".to_string(),
        package_type: "portable".to_string(),
    }))
}

fn fetch_gitee_release_manifest(
    owner: &str,
    repo: &str,
) -> Result<version::RemoteVersionManifest, String> {
    let latest_api = format!("https://gitee.com/api/v5/repos/{owner}/{repo}/releases/latest");
    let list_api = format!("https://gitee.com/api/v5/repos/{owner}/{repo}/releases?per_page=1");
    let agents = [build_ureq_agent(5, 8), build_ureq_agent_direct(5, 8)];
    let mut last_err = String::from("无法连接 Gitee Releases API");

    for agent in &agents {
        for endpoint in [&latest_api, &list_api] {
            match http_get_text(agent, endpoint, "") {
                Ok(body) => {
                    if let Ok(manifest) = version::parse_remote_manifest_body(&body) {
                        return Ok(manifest);
                    }
                }
                Err(HttpTextErr::Status(code, detail)) => {
                    last_err = format!("连接 Gitee 国内源失败：{detail} (HTTP {code})");
                }
                Err(HttpTextErr::Other(msg)) => {
                    last_err = msg;
                }
            }
        }
    }

    for agent in &agents {
        if let Some(Ok(manifest)) = fetch_public_gitee_release_via_redirect(agent, owner, repo) {
            return Ok(manifest);
        }
    }

    Err(last_err)
}

fn fetch_primary_manifest_once(
    trimmed: &str,
    token: &str,
) -> Result<version::RemoteVersionManifest, String> {
    if let Some((owner, repo)) = version::extract_gitee_owner_repo(trimmed) {
        return fetch_gitee_release_manifest(&owner, &repo);
    }

    let resolved_url = version::normalize_update_check_url(trimmed);
    let agent = build_ureq_agent(5, 8);

    match http_get_text(&agent, &resolved_url, token) {
        Ok(body) => {
            let mut manifest = version::parse_remote_manifest_body(&body)?;
            manifest.download_url =
                version::resolve_manifest_download_url(trimmed, &manifest.download_url);
            Ok(manifest)
        }
        Err(HttpTextErr::Status(code, detail)) => {
            if let Some((owner, repo)) = version::extract_github_owner_repo(&resolved_url) {
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
                            "连接更新服务器返回 404：仓库可能是私有（Private）或尚未公开发布正式 Release。"
                                .to_string(),
                        );
                    }
                    return Err(
                        "连接更新服务器返回 404：未找到可访问的 Release，或当前访问 Token 权限不足。"
                            .to_string(),
                    );
                }
                if code == 401 {
                    return Err(
                        "更新源访问令牌无效或已过期（401），请在「本地 Web 控制台 → 版本与更新」中重新配置。"
                            .to_string(),
                    );
                }
                if code == 403 && token.trim().is_empty() {
                    if let Some(res) = fetch_public_github_release_via_redirect(&agent, &owner, &repo) {
                        return res;
                    }
                    return Err(
                        "更新源 API 访问受限（403 请求频率限制），请稍后重试或改用国内 Gitee / 本地路径更新。"
                            .to_string(),
                    );
                }
            }
            Err(format!("连接更新服务器失败：{detail}"))
        }
        Err(HttpTextErr::Other(msg)) => Err(msg),
    }
}

/// 轻量级国内 HTTP/本地/局域网更新清单读取：
/// 默认直连 Gitee Releases；支持自建内网 HTTP `version.json`、本地目录及局域网共享目录（UNC）
fn fetch_remote_manifest(
    url: &str,
    token: &str,
) -> Result<(version::RemoteVersionManifest, Option<String>), String> {
    let migrated = version::migrate_legacy_update_url(url);
    let trimmed = migrated.trim();
    if trimmed.is_empty() || trimmed.contains("updates.zwai.local") {
        return Err(
            "尚未配置更新源地址：请在「本地 Web 控制台 → 版本与更新」填入 Gitee Releases 地址、内网 HTTP 或本地共享路径"
                .to_string(),
        );
    }

    if let Some(raw_local_path) = resolve_local_or_unc_path(trimmed) {
        let manifest_path = if raw_local_path.is_dir() {
            raw_local_path.join("version.json")
        } else {
            raw_local_path
        };
        let meta = std::fs::metadata(&manifest_path)
            .map_err(|e| format!("读取本地/局域网更新清单失败（{}）：{e}", manifest_path.display()))?;
        if meta.len() > 256 * 1024 {
            return Err("更新清单文件过大（超过 256 KB）".to_string());
        }
        let body = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("读取本地/局域网更新清单失败：{e}"))?;
        let mut manifest = version::parse_remote_manifest_body(&body)?;
        let manifest_src_str = manifest_path.to_string_lossy();
        manifest.download_url =
            version::resolve_manifest_download_url(&manifest_src_str, &manifest.download_url);
        return Ok((manifest, None));
    }

    match fetch_primary_manifest_once(trimmed, token) {
        Ok(manifest) => {
            let fallback_url = if version::is_official_github_release_url(trimmed) {
                version::build_gitee_mirror_exe_url(&manifest.version)
            } else {
                None
            };
            Ok((manifest, fallback_url))
        }
        Err(primary_err) => {
            if version::is_official_github_release_url(trimmed) {
                win_sys::log_warn(
                    "update",
                    &format!(
                        "GitHub source failed ({primary_err}), falling back to domestic Gitee (djjz/HalfHourChime)"
                    ),
                );
                match fetch_gitee_release_manifest("djjz", "HalfHourChime") {
                    Ok(gitee_manifest) => {
                        win_sys::log_info(
                            "update",
                            &format!(
                                "Gitee domestic check succeeded version={} url={}",
                                gitee_manifest.version, gitee_manifest.download_url
                            ),
                        );
                        return Ok((gitee_manifest, None));
                    }
                    Err(gitee_err) => {
                        return Err(format!(
                            "更新源暂不可用（Gitee 国内源：{gitee_err}）"
                        ));
                    }
                }
            }
            Err(primary_err)
        }
    }
}

fn format_release_notes_for_ui(raw: &str) -> String {
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("### 构建与交付说明") && !out.is_empty() {
            break;
        }
        if trimmed.starts_with("## 半整点提醒") || trimmed.starts_with("### 本次更新内容") {
            continue;
        }
        let clean = trimmed
            .trim_start_matches('#')
            .trim()
            .replace('`', "")
            .replace("**", "");
        if let Some(rest) = clean.strip_prefix("- ") {
            out.push(format!("• {rest}"));
        } else {
            out.push(clean);
        }
    }
    if out.is_empty() {
        "包含稳定性优化与功能改进，建议更新体验。".to_string()
    } else {
        out.join("\n")
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
        let result = fetch_remote_manifest(url, token).and_then(|(manifest, fallback_dl)| {
            let newer = version::is_newer_than_current(&manifest.version)?;
            Ok((newer, manifest, fallback_dl))
        });

        let mw_w = mw_weak.clone();
        let aw_w = aw_weak.clone();
        let uw_w = uw_weak.clone();
        let ctx_clone = ctx.clone();

        ctx_clone.is_checking.store(false, Ordering::SeqCst);
        match &result {
            Ok((true, manifest, fallback_dl)) => {
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
                if let Ok(mut fb_guard) = ctx_clone.fallback_download_url.lock() {
                    *fb_guard = fallback_dl.clone().unwrap_or_default();
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
                let notes = format_release_notes_for_ui(&manifest.notes);
                if let Ok(mut n) = ctx_clone.release_notes.lock() {
                    *n = notes;
                }
            }
            Ok((false, _, _)) => {
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
                Ok((true, manifest, _)) => {
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
                        if let Some(aw) = aw_w.upgrade() {
                            if aw.window().is_visible() {
                                let _ = aw.hide();
                            }
                        }
                        if let Some(uw) = uw_w.upgrade() {
                            uw.set_current_version(version::display_version().into());
                            uw.set_remote_version(display_remote.into());
                            if !ctx_clone.is_downloading.load(Ordering::SeqCst) {
                                uw.set_download_progress(0);
                                uw.set_download_status_text("".into());
                            }
                            let notes = format_release_notes_for_ui(&manifest.notes);
                            uw.set_release_notes(notes.into());
                            center_and_show_window(&uw, 480.0, 430.0);
                        }
                    }
                }
                Ok((false, _, _)) => {
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
                    // 后台自动巡检遇到离线或网络未连通时保持静默，不打扰日常使用；仅在手动检查时展示具体状态
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

fn bring_window_to_front<T: ComponentHandle>(win: &T) {
    let w = win.window();
    let handle_wrapper = w.window_handle();
    if let Ok(raw_handle) = handle_wrapper.window_handle() {
        if let RawWindowHandle::Win32(win32_h) = raw_handle.as_raw() {
            win_sys::bring_window_topmost(win32_h.hwnd.get());
        }
    }
}

fn resolve_chime_playback_params(
    cfg: &AppConfig,
    scheduler: &Scheduler,
    now: chrono::DateTime<Local>,
) -> (Option<String>, Option<String>, Option<String>) {
    let holiday = if cfg.holiday_enabled {
        scheduler.lookup_holiday(now.date_naive())
    } else {
        None
    };
    let tts_opt = Scheduler::build_chime_announcement_with_holiday(cfg, now, holiday.as_ref());
    let holiday_audio_opt = if cfg.holiday_enabled && holiday.is_some() {
        let p = cfg.holiday_custom_audio_path.trim();
        if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        }
    } else {
        None
    };
    (holiday_audio_opt, tts_opt, holiday.map(|h| h.name))
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
    alarm_alert_w: Option<&AlarmAlertWindow>,
    theme_idx: i32,
) {
    ThemeStore::get(main_w).set_active_index(theme_idx);
    ThemeStore::get(settings_w).set_active_index(theme_idx);
    ThemeStore::get(about_w).set_active_index(theme_idx);
    ThemeStore::get(update_w).set_active_index(theme_idx);
    if let Some(aaw) = alarm_alert_w {
        ThemeStore::get(aaw).set_active_index(theme_idx);
    }
}

/// 懒加载创建闹钟响铃强提醒弹窗，避免在仅使用整半点报时的静默驻留期间提前分配第 5 套 Slint 窗口与渲染上下文
fn ensure_alarm_alert_window(
    slot: &Rc<RefCell<Option<AlarmAlertWindow>>>,
    state_rc: &Rc<RefCell<AppState>>,
    mw_weak: &slint::Weak<MainWindow>,
    audio: &AudioEngine,
    theme_idx: i32,
) -> Option<AlarmAlertWindow> {
    {
        let guard = slot.borrow();
        if let Some(existing) = guard.as_ref() {
            ThemeStore::get(existing).set_active_index(theme_idx);
            return existing.as_weak().upgrade();
        }
    }

    let alarm_alert_win = match AlarmAlertWindow::new() {
        Ok(w) => w,
        Err(err) => {
            win_sys::log_error("ui", &format!("failed to create AlarmAlertWindow lazily: {err}"));
            return None;
        }
    };
    ThemeStore::get(&alarm_alert_win).set_active_index(theme_idx);

    {
        let w_drag = alarm_alert_win.as_weak();
        alarm_alert_win.on_drag_window(move |dx, dy| {
            if let Some(w) = w_drag.upgrade() {
                drag_slint_window(&w, dx, dy);
            }
        });
        let w_min = alarm_alert_win.as_weak();
        alarm_alert_win.on_minimize_window(move || {
            if let Some(w) = w_min.upgrade() {
                w.window().set_minimized(true);
            }
        });
    }
    {
        let aaw_weak = alarm_alert_win.as_weak();
        let st_rc = Rc::clone(state_rc);
        let mw_w = mw_weak.clone();
        let audio_clone = audio.clone();
        alarm_alert_win.window().on_close_requested(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            st.scheduler.clear_snooze();
            if let Some(aaw) = aaw_weak.upgrade() {
                let _ = aaw.hide();
            }
            if let Some(mw) = mw_w.upgrade() {
                refresh_main_window_ui(&mw, &st, false);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    {
        let aaw_weak = alarm_alert_win.as_weak();
        let st_rc = Rc::clone(state_rc);
        let mw_w = mw_weak.clone();
        let audio_clone = audio.clone();
        alarm_alert_win.on_close_clicked(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            st.scheduler.clear_snooze();
            if let Some(aaw) = aaw_weak.upgrade() {
                let _ = aaw.hide();
            }
            if let Some(mw) = mw_w.upgrade() {
                refresh_main_window_ui(&mw, &st, false);
            }
        });
    }
    {
        let aaw_weak = alarm_alert_win.as_weak();
        let st_rc = Rc::clone(state_rc);
        let mw_w = mw_weak.clone();
        let audio_clone = audio.clone();
        alarm_alert_win.on_dismiss_clicked(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            st.scheduler.clear_snooze();
            if let Some(aaw) = aaw_weak.upgrade() {
                let _ = aaw.hide();
            }
            if let Some(mw) = mw_w.upgrade() {
                refresh_main_window_ui(&mw, &st, false);
            }
        });
    }
    {
        let aaw_weak = alarm_alert_win.as_weak();
        let st_rc = Rc::clone(state_rc);
        let mw_w = mw_weak.clone();
        let audio_clone = audio.clone();
        alarm_alert_win.on_snooze_clicked(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            if let Some(alarm) = st.last_triggered_alarm.clone() {
                let trigger_at = Local::now() + chrono::Duration::minutes(5);
                st.scheduler.schedule_snooze(alarm, trigger_at);
                win_sys::log_info(
                    "scheduler",
                    &format!("alarm snoozed for 5 minutes until {}", trigger_at.format("%H:%M:%S")),
                );
            }
            if let Some(aaw) = aaw_weak.upgrade() {
                let _ = aaw.hide();
            }
            if let Some(mw) = mw_w.upgrade() {
                refresh_main_window_ui(&mw, &st, false);
            }
        });
    }

    let ret = alarm_alert_win.as_weak().upgrade();
    *slot.borrow_mut() = Some(alarm_alert_win);
    ret
}

fn sync_alarm_list_to_settings(settings_w: &SettingsWindow, alarms: &[CustomAlarm]) {
    let items: Vec<AlarmListItem> = alarms
        .iter()
        .map(|a| AlarmListItem {
            id: a.id as i32,
            label: a.label.clone().into(),
            time_text: a.format_time().into(),
            cycle_text: a.format_cycle_summary().into(),
            audio_badge: a.format_audio_badge().into(),
            enabled: a.enabled,
        })
        .collect();
    settings_w.set_alarm_items(ModelRc::new(VecModel::from(items)));
}

fn load_alarm_into_editor(settings_w: &SettingsWindow, editing_idx: i32, alarm: &CustomAlarm) {
    settings_w.set_alarm_editing_index(editing_idx);
    settings_w.set_alarm_edit_label(alarm.label.clone().into());
    settings_w.set_alarm_edit_hour(i32::from(alarm.hour.min(23)));
    settings_w.set_alarm_edit_minute(i32::from(alarm.minute.min(59)));
    settings_w.set_alarm_edit_repeat_mode(alarm.repeat_mode.to_index());
    settings_w.set_alarm_wd_0(alarm.weekdays[0]);
    settings_w.set_alarm_wd_1(alarm.weekdays[1]);
    settings_w.set_alarm_wd_2(alarm.weekdays[2]);
    settings_w.set_alarm_wd_3(alarm.weekdays[3]);
    settings_w.set_alarm_wd_4(alarm.weekdays[4]);
    settings_w.set_alarm_wd_5(alarm.weekdays[5]);
    settings_w.set_alarm_wd_6(alarm.weekdays[6]);
    settings_w.set_alarm_edit_interval_days(i32::from(alarm.interval_days.clamp(1, 365)));
    let default_today = Local::now().format("%Y-%m-%d").to_string();
    let anchor = if alarm.anchor_date.trim().is_empty() {
        default_today
    } else {
        alarm.anchor_date.clone()
    };
    settings_w.set_alarm_edit_anchor_date(anchor.into());
    settings_w.set_alarm_edit_once_date(alarm.once_date.clone().into());
    settings_w.set_alarm_edit_repeat(i32::from(alarm.repeat_count.clamp(1, 10)));
    settings_w.set_alarm_edit_play_chime(alarm.play_chime_before);
    settings_w.set_alarm_edit_tts_enabled(alarm.tts_enabled);
    settings_w.set_alarm_edit_tts_text(alarm.tts_text.clone().into());
    settings_w.set_alarm_edit_custom_audio(alarm.custom_audio_path.clone().into());
    settings_w.set_alarm_edit_respect_quiet(alarm.respect_quiet_hours);
    settings_w.set_alarm_editor_open(true);
    settings_w.set_validation_error("".into());
}

fn read_alarm_from_editor(
    settings_w: &SettingsWindow,
    existing: Option<&CustomAlarm>,
    fallback_id: u64,
) -> CustomAlarm {
    let now = Local::now();
    let mut alarm = CustomAlarm {
        id: existing.map(|a| a.id).unwrap_or(fallback_id),
        label: settings_w.get_alarm_edit_label().to_string(),
        enabled: existing.map(|a| a.enabled).unwrap_or(true),
        hour: settings_w.get_alarm_edit_hour().clamp(0, 23) as u8,
        minute: settings_w.get_alarm_edit_minute().clamp(0, 59) as u8,
        repeat_mode: AlarmRepeatMode::from_index(settings_w.get_alarm_edit_repeat_mode()),
        weekdays: [
            settings_w.get_alarm_wd_0(),
            settings_w.get_alarm_wd_1(),
            settings_w.get_alarm_wd_2(),
            settings_w.get_alarm_wd_3(),
            settings_w.get_alarm_wd_4(),
            settings_w.get_alarm_wd_5(),
            settings_w.get_alarm_wd_6(),
        ],
        interval_days: settings_w.get_alarm_edit_interval_days().clamp(1, 365) as u16,
        anchor_date: settings_w.get_alarm_edit_anchor_date().to_string(),
        once_date: settings_w.get_alarm_edit_once_date().to_string(),
        repeat_count: settings_w.get_alarm_edit_repeat().clamp(1, 10) as u8,
        play_chime_before: settings_w.get_alarm_edit_play_chime(),
        tts_enabled: settings_w.get_alarm_edit_tts_enabled(),
        tts_text: settings_w.get_alarm_edit_tts_text().to_string(),
        custom_audio_path: settings_w.get_alarm_edit_custom_audio().to_string(),
        respect_quiet_hours: settings_w.get_alarm_edit_respect_quiet(),
    };
    alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
    alarm
}

fn build_preset_alarm(preset_kind: i32, id: u64) -> CustomAlarm {
    match preset_kind {
        0 => CustomAlarm {
            id,
            label: "喝水提醒".to_string(),
            enabled: true,
            hour: 10,
            minute: 30,
            repeat_mode: AlarmRepeatMode::Daily,
            weekdays: [true; 7],
            interval_days: 1,
            anchor_date: String::new(),
            once_date: String::new(),
            repeat_count: 1,
            play_chime_before: true,
            tts_enabled: true,
            tts_text: "现在时间，{TIME}，请喝一杯水休息一下".to_string(),
            custom_audio_path: String::new(),
            respect_quiet_hours: false,
        },
        1 => CustomAlarm {
            id,
            label: "起身活动".to_string(),
            enabled: true,
            hour: 15,
            minute: 0,
            repeat_mode: AlarmRepeatMode::Workdays,
            weekdays: [true, true, true, true, true, false, false],
            interval_days: 1,
            anchor_date: String::new(),
            once_date: String::new(),
            repeat_count: 1,
            play_chime_before: true,
            tts_enabled: true,
            tts_text: "现在时间，{TIME}，久坐辛苦了，起来活动几分钟吧".to_string(),
            custom_audio_path: String::new(),
            respect_quiet_hours: false,
        },
        _ => CustomAlarm {
            id,
            label: "下班打卡提醒".to_string(),
            enabled: true,
            hour: 18,
            minute: 0,
            repeat_mode: AlarmRepeatMode::Workdays,
            weekdays: [true, true, true, true, true, false, false],
            interval_days: 1,
            anchor_date: String::new(),
            once_date: String::new(),
            repeat_count: 2,
            play_chime_before: true,
            tts_enabled: true,
            tts_text: "现在时间，{TIME}，请记得下班打卡".to_string(),
            custom_audio_path: String::new(),
            respect_quiet_hours: false,
        },
    }
}

fn populate_settings_form(
    settings_w: &SettingsWindow,
    cfg: &AppConfig,
    scheduler: &Scheduler,
) {
    settings_w.set_hourly_chime_enabled(cfg.hourly_chime_enabled);
    settings_w.set_half_hour_chime_enabled(cfg.half_hour_chime_enabled);
    settings_w.set_extra_chime_minutes_str(cfg.format_custom_chime_minutes().into());
    settings_w.set_chime_repeat(cfg.chime_repeat_count as i32);
    settings_w.set_chime_play_sound(cfg.chime_play_sound);
    settings_w.set_chime_tts_enabled(cfg.chime_tts_enabled);
    settings_w.set_chime_tts_template(cfg.chime_tts_template.clone().into());

    settings_w.set_holiday_enabled(cfg.holiday_enabled);
    settings_w.set_holiday_custom_audio_path(cfg.holiday_custom_audio_path.clone().into());
    settings_w.set_holiday_greeting_template(cfg.holiday_greeting_template.clone().into());
    settings_w.set_holiday_today_preview(
        scheduler
            .describe_day_calendar(Local::now().date_naive())
            .into(),
    );

    settings_w.set_quiet_enabled(cfg.quiet_enabled);
    settings_w.set_quiet_start_hour(cfg.quiet_start_hour as i32);
    settings_w.set_quiet_start_minute(cfg.quiet_start_minute as i32);
    settings_w.set_quiet_end_hour(cfg.quiet_end_hour as i32);
    settings_w.set_quiet_end_minute(cfg.quiet_end_minute as i32);

    sync_alarm_list_to_settings(settings_w, &cfg.alarms);
    settings_w.set_alarm_editor_open(false);
    settings_w.set_alarm_editing_index(-1);

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

fn refresh_main_window_ui(main_w: &MainWindow, state: &AppState, is_audio_playing: bool) {
    let now = Local::now();
    let today = now.date_naive();
    let time_str = now.format("%H:%M:%S").to_string();
    let lunar_part = solar_to_lunar(today)
        .map(|l| format!(" · {}", l.format_zh()))
        .unwrap_or_default();
    let holiday_part = if state.config.holiday_enabled {
        state
            .scheduler
            .lookup_holiday(today)
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

    let next = state.scheduler.compute_next_event(&state.config, now);
    let summary = state.config.format_chime_summary();

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
    main_w.set_is_audio_playing(is_audio_playing);
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
        refresh_main_window_ui(main_w, &st, false);
        settings_w.set_is_syncing(true);
        target
    };

    let state_weak_tx = main_w.as_weak();
    let settings_weak_tx = settings_w.as_weak();

    win_sys::log_info("ntp", &format!("starting NTP sync server={server}"));
    let _ = std::thread::Builder::new()
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
        version::DEFAULT_UPDATE_URL.to_string()
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

/// 执行“自动更新”：流式下载临时安装包 -> 校验 PE 可执行头部 -> 原地无感替换（免 cmd/ping 窗口）-> 释放锁与端口并重启
fn download_and_apply_auto_update(
    ctx: &UpdateContext,
    update_w: &UpdateWindow,
    update_tx: &Sender<UpdateWorkerMsg>,
    perf_stop_tx: &Sender<()>,
    web_console: &Rc<RefCell<WebConsoleController>>,
    audio: &AudioEngine,
    single_instance_guard: &Rc<RefCell<Option<win_sys::SingleInstanceGuard>>>,
) {
    let raw_url = ctx
        .download_url
        .lock()
        .map(|g| g.trim().to_string())
        .unwrap_or_default();
    let fallback_dl_url = ctx
        .fallback_download_url
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
        center_and_show_window(update_w, 480.0, 430.0);
        return;
    }

    if ctx
        .is_downloading
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        center_and_show_window(update_w, 480.0, 430.0);
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
    center_and_show_window(update_w, 480.0, 430.0);
    let ctx_clone = ctx.clone();
    let u_tx_clone = update_tx.clone();
    let perf_tx_clone = perf_stop_tx.clone();
    let audio_clone = audio.clone();
    // 使用通道通知主线程执行优雅退出与平滑重启（确保在主线程释放互斥锁与 Web 端口后再拉起新版）
    let (restart_tx, restart_rx) = mpsc::channel::<win_sys::UpdateLaunchPlan>();
    {
        let guard_rc = Rc::clone(single_instance_guard);
        let wc_exit = Rc::clone(web_console);
        let uw_w_exit = uw_weak.clone();
        let ctx_exit = ctx.clone();
        let u_tx_exit = u_tx_clone.clone();
        let perf_tx_exit = perf_tx_clone.clone();
        let audio_exit = audio_clone.clone();
        let restart_poll = Timer::default();
        restart_poll.start(TimerMode::Repeated, Duration::from_millis(80), move || {
            if let Ok(plan) = restart_rx.try_recv() {
                let _ = u_tx_exit.send(UpdateWorkerMsg::Shutdown);
                let _ = perf_tx_exit.send(());
                wc_exit.borrow_mut().stop();
                audio_exit.shutdown();
                // 必须在拉起新版前释放单实例互斥锁，防止新版本被误判为重复启动
                let _ = guard_rc.borrow_mut().take();
                win_sys::flush_logs();
                if let Err(err) = win_sys::execute_update_launch_plan(&plan) {
                    win_sys::log_error("updater", &format!("execute_update_launch_plan failed: {err}"));
                    ctx_exit.is_downloading.store(false, Ordering::SeqCst);
                    if let Ok(mut ds) = ctx_exit.download_status_text.lock() {
                        *ds = err.clone();
                    }
                    if let Some(uw) = uw_w_exit.upgrade() {
                        uw.set_is_downloading(false);
                        uw.set_download_progress(0);
                        uw.set_download_status_text(err.into());
                    }
                    return;
                }
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
                    let download_via_http = |target_url: &str,
                                             target_api_url: &str,
                                             target_token: &str,
                                             use_proxy: bool|
                     -> Result<(), String> {
                        let agent = build_ureq_agent_with_options(6, 60, 5, use_proxy);
                        let resp = if !target_token.is_empty() && !target_api_url.is_empty() {
                            let no_redir = build_ureq_agent_with_options(6, 60, 0, use_proxy);
                            let api_resp = no_redir
                                .get(target_api_url)
                                .set(
                                    "User-Agent",
                                    &format!("HalfHourChime/{}", version::APP_PKG_VERSION),
                                )
                                .set("Accept", "application/octet-stream")
                                .set("Authorization", &format!("Bearer {target_token}"))
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
                                .get(target_url)
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
                                    "更新包体积超出安全上限（100 MB），已中止下载。".to_string(),
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
                                drop(file);
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
                                    let status_str =
                                        format!("正在下载更新包（已下载 {mb:.2} MB）...");
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
                        Ok(())
                    };

                    let primary_res = match download_via_http(&url, &api_asset_url, &gh_token, true) {
                        Ok(()) => Ok(()),
                        Err(proxy_err) if url.contains("gitee.com") => {
                            win_sys::log_warn(
                                "updater",
                                &format!(
                                    "Gitee download with proxy failed ({proxy_err}), retrying direct connection"
                                ),
                            );
                            download_via_http(&url, "", "", false).or(Err(proxy_err))
                        }
                        Err(err) => Err(err),
                    };

                    if let Err(primary_dl_err) = primary_res {
                        if !fallback_dl_url.is_empty() && fallback_dl_url != url {
                            win_sys::log_warn(
                                "updater",
                                &format!(
                                    "primary download failed ({primary_dl_err}), falling back to mirror: {fallback_dl_url}"
                                ),
                            );
                            let switch_msg = if fallback_dl_url.contains("gitee.com") {
                                "主下载源连接受限，正在自动切换国内 Gitee 镜像源下载...".to_string()
                            } else {
                                "主下载源连接受限，正在尝试备用镜像源下载...".to_string()
                            };
                            ctx_clone.download_progress.store(0, Ordering::SeqCst);
                            if let Ok(mut ds) = ctx_clone.download_status_text.lock() {
                                *ds = switch_msg.clone();
                            }
                            let uw_w = uw_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(uw) = uw_w.upgrade() {
                                    uw.set_download_progress(0);
                                    uw.set_download_status_text(switch_msg.into());
                                }
                            });

                            if let Err(fb_proxy_err) = download_via_http(
                                &fallback_dl_url,
                                &api_asset_url,
                                &gh_token,
                                true,
                            ) {
                                if fallback_dl_url.contains("gitee.com") {
                                    win_sys::log_warn(
                                        "updater",
                                        &format!(
                                            "Gitee mirror download with proxy failed ({fb_proxy_err}), retrying direct connection"
                                        ),
                                    );
                                    download_via_http(&fallback_dl_url, "", "", false)?;
                                } else {
                                    return Err(fb_proxy_err);
                                }
                            }
                        } else {
                            return Err(primary_dl_err);
                        }
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

                    let launch_plan = match win_sys::prepare_update_and_build_launch(
                        &temp_pkg_path,
                        is_installer_mode,
                    ) {
                        Ok(plan) => plan,
                        Err(err) => {
                            let _ = std::fs::remove_file(&temp_pkg_path);
                            ctx_clone.is_downloading.store(false, Ordering::SeqCst);
                            if let Ok(mut ds) = ctx_clone.download_status_text.lock() {
                                *ds = err.clone();
                            }
                            let uw_w2 = uw_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(uw) = uw_w2.upgrade() {
                                    uw.set_is_downloading(false);
                                    uw.set_download_status_text(err.into());
                                }
                            });
                            return;
                        }
                    };

                    // 通知主线程释放单实例锁与本地端口并无缝重启新版本
                    win_sys::log_info("updater", "temp package verified, signaling seamless restart");
                    win_sys::flush_logs();
                    let _ = restart_tx.send(launch_plan);
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
        hourly_chime_enabled: cfg.hourly_chime_enabled,
        half_hour_chime_enabled: cfg.half_hour_chime_enabled,
        custom_chime_minutes_str: cfg.format_custom_chime_minutes(),
        chime_repeat_count: cfg.chime_repeat_count,
        chime_play_sound: cfg.chime_play_sound,
        chime_tts_enabled: cfg.chime_tts_enabled,
        chime_tts_template: cfg.chime_tts_template.clone(),
        holiday_enabled: cfg.holiday_enabled,
        holiday_greeting_template: cfg.holiday_greeting_template.clone(),
        holiday_custom_audio_path: cfg.holiday_custom_audio_path.clone(),
        quiet_enabled: cfg.quiet_enabled,
        quiet_start_hour: cfg.quiet_start_hour,
        quiet_start_minute: cfg.quiet_start_minute,
        quiet_end_hour: cfg.quiet_end_hour,
        quiet_end_minute: cfg.quiet_end_minute,
        alarms: cfg.alarms.clone(),
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
    if let Some(v) = patch.hourly_chime_enabled {
        next.hourly_chime_enabled = v;
    }
    if let Some(v) = patch.half_hour_chime_enabled {
        next.half_hour_chime_enabled = v;
    }
    if let Some(ref mins) = patch.custom_chime_minutes_str {
        next.custom_chime_minutes = AppConfig::parse_optional_extra_minutes(mins)?;
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
    if let Some(mut alarms) = patch.alarms {
        let now = Local::now();
        for alarm in &mut alarms {
            alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
        }
        next.alarms = alarms;
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
    // 0. 检查是否为按需 UAC 提权子进程模式（--sync-ntp 或 --apply-elevated-update）
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--sync-ntp") {
        let server = args
            .get(pos + 1)
            .map(|s| s.as_str())
            .unwrap_or("ntp.aliyun.com");
        let code = ntp::run_elevated_cli_mode(server);
        std::process::exit(code);
    }
    if let Some(pos) = args.iter().position(|a| a == "--apply-elevated-update") {
        if let (Some(src), Some(dst)) = (args.get(pos + 1), args.get(pos + 2)) {
            let code = win_sys::run_elevated_update_apply_mode(
                std::path::Path::new(src),
                std::path::Path::new(dst),
            );
            std::process::exit(code);
        }
        std::process::exit(1);
    }

    // 1. 单实例互斥锁检查（FR-24 / AC-09），支持自动更新重启前主动释放
    let single_instance_guard = Rc::new(RefCell::new(match win_sys::acquire_single_instance() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            return;
        }
        Err(_) => None,
    }));

    let cleanup_temp_arg = args
        .iter()
        .position(|a| a == "--cleanup-temp")
        .and_then(|pos| args.get(pos + 1).cloned());
    win_sys::spawn_cleanup_stale_update_files(cleanup_temp_arg);

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

    // 5. 构建 Slint 原生多窗口组件（默认隐藏，仅托盘可见；闹钟弹窗按需懒加载初始化以节省常驻内存）
    let main_win = MainWindow::new().expect("Failed to create MainWindow");
    let settings_win = SettingsWindow::new().expect("Failed to create SettingsWindow");
    let about_win = AboutWindow::new().expect("Failed to create AboutWindow");
    let update_win = UpdateWindow::new().expect("Failed to create UpdateWindow");
    let alarm_alert_slot: Rc<RefCell<Option<AlarmAlertWindow>>> = Rc::new(RefCell::new(None));

    about_win.set_app_version(version::display_version().into());
    about_win.set_app_vendor(version::APP_VENDOR.into());
    about_win.set_config_path(config_store.path.to_string_lossy().to_string().into());
    update_win.set_current_version(version::display_version().into());
    sync_theme_across_windows(
        &main_win,
        &settings_win,
        &about_win,
        &update_win,
        None,
        cfg.theme.to_index(),
    );

    let initial_ntp_text = load_warning.unwrap_or_else(|| "校时状态：尚未校时".to_string());
    let initial_volume = cfg.volume;
    let initial_auto_start = cfg.auto_start;
    let initial_mute_mode = cfg.mute_mode;
    let initial_update_enabled = cfg.update_check_enabled;
    let initial_update_url = if cfg.update_check_url.trim().is_empty() {
        version::DEFAULT_UPDATE_URL.to_string()
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
        tts_preview_status: String::new(),
        last_triggered_alarm: None,
    }));

    let draft_alarms: Rc<RefCell<Vec<CustomAlarm>>> =
        Rc::new(RefCell::new(state.borrow().config.alarms.clone()));

    refresh_main_window_ui(&main_win, &state.borrow(), false);

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
        *st_msg = "已启用 Gitee 国内直连 / 本地与局域网自动更新检查".to_string();
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

    // 6. 初始化系统托盘图标与右键上下文菜单（FR-14 / FR-15 / 快捷控制）
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
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
                sync_theme_across_windows(
                    &mw,
                    &sw,
                    &aw,
                    &uw,
                    aaw_slot.borrow().as_ref(),
                    st.config.theme.to_index(),
                );
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
    // 7.5 绑定无边框窗口的自定义标题栏拖拽、最小化与关闭按钮控制
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
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
                sync_theme_across_windows(
                    &mw,
                    &sw,
                    &aw,
                    &uw,
                    aaw_slot.borrow().as_ref(),
                    st.config.theme.to_index(),
                );
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
        let mw_weak = main_win.as_weak();
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
            if let Some(mw) = mw_weak.upgrade() {
                mw.set_is_audio_playing(true);
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let mw_weak = main_win.as_weak();
        let aaw_slot = Rc::clone(&alarm_alert_slot);
        let audio_clone = audio.clone();
        main_win.on_stop_audio(move || {
            audio_clone.stop_active();
            let mut st = st_rc.borrow_mut();
            st.scheduler.clear_snooze();
            if let Some(aaw) = aaw_slot.borrow().as_ref() {
                let _ = aaw.hide();
            }
            if let Some(mw) = mw_weak.upgrade() {
                refresh_main_window_ui(&mw, &st, false);
            }
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
        let drafts_rc = Rc::clone(&draft_alarms);
        main_win.on_open_settings(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let st = st_rc.borrow();
                *drafts_rc.borrow_mut() = st.config.alarms.clone();
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
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
                sync_theme_across_windows(
                    &mw,
                    &sw,
                    &aw,
                    &uw,
                    aaw_slot.borrow().as_ref(),
                    next_idx,
                );
            }
        });
    }
    {
        let ctx_clone = update_ctx.clone();
        let uw_weak = update_win.as_weak();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let wc_rc = Rc::clone(&web_console);
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        main_win.on_download_update(move || {
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &wc_rc,
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
                preview_cfg.hourly_chime_enabled = sw.get_hourly_chime_enabled();
                preview_cfg.half_hour_chime_enabled = sw.get_half_hour_chime_enabled();
                preview_cfg.chime_play_sound = sw.get_chime_play_sound();
                preview_cfg.chime_tts_enabled = sw.get_chime_tts_enabled();
                preview_cfg.chime_tts_template = sw.get_chime_tts_template().to_string();
                preview_cfg.holiday_enabled = sw.get_holiday_enabled();
                preview_cfg.holiday_custom_audio_path =
                    sw.get_holiday_custom_audio_path().to_string();
                preview_cfg.holiday_greeting_template =
                    sw.get_holiday_greeting_template().to_string();
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
                audio_clone.reset_edge_tts_cooldown();
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
                    audio_clone.reset_edge_tts_cooldown();
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
            } else {
                audio_clone.reset_edge_tts_cooldown();
                if let Some(sw) = sw_weak.upgrade() {
                    sw.set_tts_preview_status("".into());
                }
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
                let play_sound = sw.get_chime_play_sound();
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;

                let today_holiday = st
                    .scheduler
                    .lookup_holiday(now.date_naive())
                    .unwrap_or_else(|| HolidayInfo {
                        name: "春节".to_string(),
                        kind: HolidayKind::Statutory,
                        greeting: "新春大吉，祝您新年快乐、万事顺意！".to_string(),
                    });
                let custom_audio = {
                    let p = preview_cfg.holiday_custom_audio_path.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                };
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

    // 自定义闹钟管理、快捷预设、试听与编辑回调
    {
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_create_alarm(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let next_id = drafts_rc
                    .borrow()
                    .iter()
                    .map(|a| a.id)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let now = Local::now();
                let mut alarm = CustomAlarm {
                    id: next_id,
                    ..CustomAlarm::default()
                };
                alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
                load_alarm_into_editor(&sw, -1, &alarm);
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_add_preset_alarm(move |kind| {
            if let Some(sw) = sw_weak.upgrade() {
                let now = Local::now();
                let mut drafts = drafts_rc.borrow_mut();
                let next_id = drafts.iter().map(|a| a.id).max().unwrap_or(0) + 1;
                let mut alarm = build_preset_alarm(kind, next_id);
                alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
                let new_idx = drafts.len() as i32;
                drafts.push(alarm.clone());
                sync_alarm_list_to_settings(&sw, &drafts);
                load_alarm_into_editor(&sw, new_idx, &alarm);
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_edit_alarm(move |idx| {
            if let Some(sw) = sw_weak.upgrade() {
                let drafts = drafts_rc.borrow();
                if let Some(alarm) = idx.try_into().ok().and_then(|u: usize| drafts.get(u)) {
                    load_alarm_into_editor(&sw, idx, alarm);
                }
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_toggle_alarm_enabled(move |idx, next_enabled| {
            if let Some(sw) = sw_weak.upgrade() {
                let mut drafts = drafts_rc.borrow_mut();
                if let Some(alarm) = idx.try_into().ok().and_then(|u: usize| drafts.get_mut(u)) {
                    alarm.enabled = next_enabled;
                    if next_enabled && alarm.repeat_mode == AlarmRepeatMode::Once {
                        let now = Local::now();
                        alarm.ensure_anchor_and_once_dates(
                            now.date_naive(),
                            now.hour(),
                            now.minute(),
                        );
                    }
                    sync_alarm_list_to_settings(&sw, &drafts);
                }
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        let audio_clone = audio.clone();
        settings_win.on_test_alarm_item(move |idx| {
            let vol = st_rc.borrow().live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let drafts = drafts_rc.borrow();
                if let Some(alarm) = idx.try_into().ok().and_then(|u: usize| drafts.get(u)) {
                    let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                    let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                    let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                    let custom_opt = if alarm.custom_audio_path.trim().is_empty() {
                        None
                    } else {
                        Some(alarm.custom_audio_path.clone())
                    };
                    let tts_opt = Scheduler::build_alarm_announcement(alarm, Local::now());
                    audio_clone.play_alarm(
                        1,
                        vol,
                        custom_opt,
                        alarm.play_chime_before,
                        tts_opt,
                        tts_engine,
                        edge_voice,
                        tts_rate,
                    );
                }
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_delete_alarm(move |idx| {
            if let Some(sw) = sw_weak.upgrade() {
                let Ok(u_idx) = usize::try_from(idx) else {
                    return;
                };
                let mut drafts = drafts_rc.borrow_mut();
                if u_idx < drafts.len() {
                    drafts.remove(u_idx);
                    let cur_edit = sw.get_alarm_editing_index();
                    if sw.get_alarm_editor_open() {
                        if cur_edit == idx {
                            sw.set_alarm_editor_open(false);
                            sw.set_alarm_editing_index(-1);
                        } else if cur_edit > idx {
                            sw.set_alarm_editing_index(cur_edit - 1);
                        }
                    }
                    sync_alarm_list_to_settings(&sw, &drafts);
                    sw.set_validation_error("".into());
                }
            }
        });
    }
    {
        let st_rc = Rc::clone(&state);
        let sw_weak = settings_win.as_weak();
        let drafts_rc = Rc::clone(&draft_alarms);
        let audio_clone = audio.clone();
        settings_win.on_test_editing_alarm(move || {
            let vol = st_rc.borrow().live_volume;
            if let Some(sw) = sw_weak.upgrade() {
                let drafts = drafts_rc.borrow();
                let edit_idx = sw.get_alarm_editing_index();
                let existing = usize::try_from(edit_idx)
                    .ok()
                    .and_then(|u| drafts.get(u));
                let fallback_id = drafts.iter().map(|a| a.id).max().unwrap_or(0) + 1;
                let alarm = read_alarm_from_editor(&sw, existing, fallback_id);
                let tts_engine = TtsEngine::from_index(sw.get_tts_engine_index());
                let edge_voice = edge_voice_from_index(sw.get_edge_voice_index()).to_string();
                let tts_rate = sw.get_tts_rate().clamp(-5, 5) as i8;
                let custom_opt = if alarm.custom_audio_path.trim().is_empty() {
                    None
                } else {
                    Some(alarm.custom_audio_path.clone())
                };
                let tts_opt = Scheduler::build_alarm_announcement(&alarm, Local::now());
                audio_clone.play_alarm(
                    1,
                    vol,
                    custom_opt,
                    alarm.play_chime_before,
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
        let drafts_rc = Rc::clone(&draft_alarms);
        settings_win.on_commit_alarm_editor(move || {
            if let Some(sw) = sw_weak.upgrade() {
                let mut drafts = drafts_rc.borrow_mut();
                let edit_idx = sw.get_alarm_editing_index();
                let valid_idx = usize::try_from(edit_idx)
                    .ok()
                    .filter(|&u| u < drafts.len());
                let fallback_id = drafts.iter().map(|a| a.id).max().unwrap_or(0) + 1;
                let mut edited = read_alarm_from_editor(
                    &sw,
                    valid_idx.and_then(|u| drafts.get(u)),
                    fallback_id,
                );
                if let Err(err) = edited.validate() {
                    sw.set_validation_error(err.into());
                    return;
                }
                if let Some(u) = valid_idx {
                    drafts[u] = edited;
                } else {
                    drafts.push(edited);
                }
                sw.set_alarm_editor_open(false);
                sw.set_alarm_editing_index(-1);
                sw.set_validation_error("".into());
                sync_alarm_list_to_settings(&sw, &drafts);
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        settings_win.on_cancel_alarm_editor(move || {
            if let Some(sw) = sw_weak.upgrade() {
                sw.set_alarm_editor_open(false);
                sw.set_alarm_editing_index(-1);
                sw.set_validation_error("".into());
            }
        });
    }
    {
        let sw_weak = settings_win.as_weak();
        settings_win.on_browse_alarm_audio(move || {
            if let Some(path) = win_sys::browse_audio_file() {
                if let Some(sw) = sw_weak.upgrade() {
                    sw.set_alarm_edit_custom_audio(path.into());
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
        settings_win.on_select_theme(move |idx| {
            if let (Some(mw), Some(sw), Some(aw), Some(uw)) = (
                mw_weak.upgrade(),
                sw_weak.upgrade(),
                aw_weak.upgrade(),
                uw_weak.upgrade(),
            ) {
                sync_theme_across_windows(&mw, &sw, &aw, &uw, aaw_slot.borrow().as_ref(), idx);
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
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
                sync_theme_across_windows(
                    &mw,
                    &sw,
                    &aw,
                    &uw,
                    aaw_slot.borrow().as_ref(),
                    st.config.theme.to_index(),
                );
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
        let aaw_slot = Rc::clone(&alarm_alert_slot);
        let drafts_rc = Rc::clone(&draft_alarms);
        let tray_clone = Arc::clone(&tray);
        let audio_clone = audio.clone();
        let u_tx = update_tx.clone();
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

            // 若用户打开了闹钟编辑器但未先点“确认闹钟”，点保存时自动完成校验并提交该闹钟
            if sw.get_alarm_editor_open() {
                let mut drafts = drafts_rc.borrow_mut();
                let edit_idx = sw.get_alarm_editing_index();
                let valid_idx = usize::try_from(edit_idx)
                    .ok()
                    .filter(|&u| u < drafts.len());
                let fallback_id = drafts.iter().map(|a| a.id).max().unwrap_or(0) + 1;
                let mut edited = read_alarm_from_editor(
                    &sw,
                    valid_idx.and_then(|u| drafts.get(u)),
                    fallback_id,
                );
                if let Err(err) = edited.validate() {
                    sw.set_validation_error(err.into());
                    return;
                }
                if let Some(u) = valid_idx {
                    drafts[u] = edited;
                } else {
                    drafts.push(edited);
                }
                sw.set_alarm_editor_open(false);
                sw.set_alarm_editing_index(-1);
                sync_alarm_list_to_settings(&sw, &drafts);
            }

            let raw_extra_minutes = sw.get_extra_chime_minutes_str().to_string();
            let parsed_extra_minutes =
                match AppConfig::parse_optional_extra_minutes(&raw_extra_minutes) {
                    Ok(v) => v,
                    Err(err) => {
                        sw.set_validation_error(err.into());
                        return;
                    }
                };

            let (
                prev_cfg,
                preserved_mute_mode,
                preserved_update_url,
                preserved_update_token,
                preserved_update_interval,
            ) = {
                let st = st_rc.borrow();
                let u_url = if st.config.update_check_url.trim().is_empty() {
                    version::DEFAULT_UPDATE_URL.to_string()
                } else {
                    st.config.update_check_url.clone()
                };
                (
                    st.config.clone(),
                    st.config.mute_mode,
                    u_url,
                    st.config.update_github_token.clone(),
                    st.config.update_check_interval_hours.clamp(1, 168),
                )
            };

            let now = Local::now();
            let mut alarms = drafts_rc.borrow().clone();
            for alarm in &mut alarms {
                alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
            }

            let mut candidate = AppConfig {
                hourly_chime_enabled: sw.get_hourly_chime_enabled(),
                half_hour_chime_enabled: sw.get_half_hour_chime_enabled(),
                custom_chime_minutes: parsed_extra_minutes,
                chime_repeat_count: sw.get_chime_repeat().clamp(1, 10) as u8,
                chime_play_sound: sw.get_chime_play_sound(),
                chime_tts_enabled: sw.get_chime_tts_enabled(),
                chime_tts_template: sw.get_chime_tts_template().to_string(),
                holiday_enabled: sw.get_holiday_enabled(),
                holiday_custom_audio_path: sw.get_holiday_custom_audio_path().to_string(),
                holiday_greeting_template: sw.get_holiday_greeting_template().to_string(),
                mute_mode: preserved_mute_mode,
                quiet_enabled: sw.get_quiet_enabled(),
                quiet_start_hour: sw.get_quiet_start_hour().clamp(0, 23) as u8,
                quiet_start_minute: sw.get_quiet_start_minute().clamp(0, 59) as u8,
                quiet_end_hour: sw.get_quiet_end_hour().clamp(0, 23) as u8,
                quiet_end_minute: sw.get_quiet_end_minute().clamp(0, 59) as u8,
                alarms,
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

            if candidate.auto_start != prev_cfg.auto_start {
                if let Err(err) = win_sys::apply_auto_start(candidate.auto_start) {
                    sw.set_validation_error(err.into());
                    return;
                }
            }

            {
                let mut st = st_rc.borrow_mut();
                if let Err(err) = st.config_store.save(&candidate) {
                    sw.set_validation_error(err.into());
                    return;
                }
                st.config = candidate.clone();
                st.live_volume = candidate.volume;
                st.scheduler
                    .sync_slots_after_config_save(&prev_cfg, &candidate);
                refresh_main_window_ui(&mw, &st, audio_clone.is_busy());
            }
            wc_rc
                .borrow()
                .update_config_view(config_to_web_view(&candidate));

            let _ = u_tx.send(UpdateWorkerMsg::Reconfigure {
                enabled: candidate.update_check_enabled,
                url: candidate.update_check_url.clone(),
                token: candidate.update_github_token.clone(),
                interval_hours: candidate.update_check_interval_hours,
            });

            tray_clone.set_auto_start_checked(candidate.auto_start);
            audio_clone.set_live_volume(candidate.volume);
            sync_theme_across_windows(
                &mw,
                &sw,
                &aw,
                &uw,
                aaw_slot.borrow().as_ref(),
                candidate.theme.to_index(),
            );
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
        let aw_weak = about_win.as_weak();
        let uw_weak = update_win.as_weak();
        let u_tx = update_tx.clone();
        let perf_tx = perf_stop_tx.clone();
        let wc_rc = Rc::clone(&web_console);
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        about_win.on_download_update(move || {
            if let Some(aw) = aw_weak.upgrade() {
                let _ = aw.hide();
            }
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &wc_rc,
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
        let wc_rc = Rc::clone(&web_console);
        let audio_clone = audio.clone();
        let guard_rc = Rc::clone(&single_instance_guard);
        update_win.on_auto_update_clicked(move || {
            if let Some(uw) = uw_weak.upgrade() {
                download_and_apply_auto_update(
                    &ctx_clone,
                    &uw,
                    &u_tx,
                    &perf_tx,
                    &wc_rc,
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
        let aaw_slot_tray = Rc::clone(&alarm_alert_slot);
        let st_rc_main_thread = Rc::clone(&state);
        let drafts_rc_tray = Rc::clone(&draft_alarms);
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
                    refresh_main_window_ui(&mw, &st_rc_main_thread.borrow(), audio_clone.is_busy());
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
                } else if id == tray_clone.ids.stop_audio {
                    audio_clone.stop_active();
                    let mut st = st_rc_main_thread.borrow_mut();
                    st.scheduler.clear_snooze();
                    if let Some(aaw) = aaw_slot_tray.borrow().as_ref() {
                        let _ = aaw.hide();
                    }
                    if let Some(mw) = mw_weak.upgrade() {
                        refresh_main_window_ui(&mw, &st, false);
                    }
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
                        refresh_main_window_ui(&mw, &st, audio_clone.is_busy());
                    }
                } else if id == tray_clone.ids.open_settings {
                    if let Some(sw) = sw_weak.upgrade() {
                        let st = st_rc_main_thread.borrow();
                        *drafts_rc_tray.borrow_mut() = st.config.alarms.clone();
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
        let aaw_slot_tick = Rc::clone(&alarm_alert_slot);
        let drafts_tick = Rc::clone(&draft_alarms);
        let audio_clone = audio.clone();
        let tray_for_tick = Arc::clone(&tray);
        let ipc_wake_for_tick = Arc::clone(&ipc_wake_pending);
        let preview_gen_tick = Rc::clone(&last_voice_preview_gen);
        let wc_tick = Rc::clone(&web_console);
        let perf_sample_tick = Arc::clone(&latest_perf);
        let cpu_throttled_tick = Arc::clone(&cpu_throttled);
        let update_ctx_tick = update_ctx.clone();
        let u_tx_tick = update_tx.clone();
        let perf_stop_tick = perf_stop_tx.clone();
        let guard_tick = Rc::clone(&single_instance_guard);
        let mut prev_audio_busy = false;

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
                    WebAction::StopAudio => {
                        audio_clone.stop_active();
                        let mut st = st_rc.borrow_mut();
                        st.scheduler.clear_snooze();
                        if let Some(aaw) = aaw_slot_tick.borrow().as_ref() {
                            let _ = aaw.hide();
                        }
                        if let Some(mw) = mw_weak.upgrade() {
                            refresh_main_window_ui(&mw, &st, false);
                        }
                    }
                    WebAction::TestVoice {
                        tts_engine_index,
                        edge_voice_index,
                        tts_rate,
                    } => {
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
                        audio_clone.reset_edge_tts_cooldown();
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
                    WebAction::TestAlarm {
                        alarm_id,
                        inline_alarm,
                    } => {
                        let st = st_rc.borrow();
                        let alarm = inline_alarm
                            .or_else(|| {
                                alarm_id.and_then(|id| {
                                    st.config.alarms.iter().find(|a| a.id == id).cloned()
                                })
                            })
                            .or_else(|| st.config.alarms.first().cloned())
                            .unwrap_or_default();
                        let custom_opt = if alarm.custom_audio_path.trim().is_empty() {
                            None
                        } else {
                            Some(alarm.custom_audio_path.clone())
                        };
                        let tts_opt = Scheduler::build_alarm_announcement(&alarm, now);
                        audio_clone.play_alarm(
                            1,
                            st.live_volume,
                            custom_opt,
                            alarm.play_chime_before,
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
                            refresh_main_window_ui(&mw, &st, audio_clone.is_busy());
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
                            sync_theme_across_windows(
                                &mw,
                                &sw,
                                &aw,
                                &uw,
                                aaw_slot_tick.borrow().as_ref(),
                                norm_idx,
                            );
                            sw.set_theme_index(norm_idx);
                        }
                        wc_tick.borrow().update_config_view(config_to_web_view(&st.config));
                    }
                    WebAction::TrimMemory(reply_tx) => {
                        let msg = perf_guard::trim_memory_now(&audio_clone);
                        let _ = reply_tx.send(Ok(msg));
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
                                &wc_tick,
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
                            let prev_cfg = st_rc.borrow().config.clone();
                            let candidate = apply_web_config_patch(&prev_cfg, patch)?;
                            if candidate.auto_start != prev_cfg.auto_start {
                                win_sys::apply_auto_start(candidate.auto_start)?;
                            }
                            {
                                let mut st = st_rc.borrow_mut();
                                st.config_store.save(&candidate)?;
                                st.config = candidate.clone();
                                st.live_volume = candidate.volume;
                                st.scheduler
                                    .sync_slots_after_config_save(&prev_cfg, &candidate);
                                *drafts_tick.borrow_mut() = st.config.alarms.clone();
                                if let Some(mw) = mw_weak.upgrade() {
                                    refresh_main_window_ui(&mw, &st, audio_clone.is_busy());
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
                                    aaw_slot_tick.borrow().as_ref(),
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
            if st.scheduler.needs_leak_sample_at(now) {
                let mut visible_windows_mask = 0u8;
                if mw_weak.upgrade().is_some_and(|w| w.window().is_visible()) {
                    visible_windows_mask |= 1 << 0;
                }
                if sw_weak.upgrade().is_some_and(|w| w.window().is_visible()) {
                    visible_windows_mask |= 1 << 1;
                }
                if aw_weak.upgrade().is_some_and(|w| w.window().is_visible()) {
                    visible_windows_mask |= 1 << 2;
                }
                if uw_weak.upgrade().is_some_and(|w| w.window().is_visible()) {
                    visible_windows_mask |= 1 << 3;
                }
                let alarm_alert_alive = {
                    let aaw_b = aaw_slot_tick.borrow();
                    if aaw_b.as_ref().is_some_and(|w| w.window().is_visible()) {
                        visible_windows_mask |= 1 << 4;
                    }
                    aaw_b.is_some()
                };
                st.scheduler.set_runtime_refs(ScheduledRefContext {
                    app_state_strong: Rc::strong_count(&st_rc),
                    draft_alarms_strong: Rc::strong_count(&drafts_tick),
                    web_console_strong: Rc::strong_count(&wc_tick),
                    alarm_alert_slot_strong: Rc::strong_count(&aaw_slot_tick),
                    single_instance_guard_strong: Rc::strong_count(&guard_tick),
                    voice_preview_gen_strong: Rc::strong_count(&preview_gen_tick),
                    tray_arc_strong: Arc::strong_count(&tray_for_tick),
                    ipc_wake_arc_strong: Arc::strong_count(&ipc_wake_for_tick),
                    perf_sample_arc_strong: Arc::strong_count(&perf_sample_tick),
                    cpu_throttled_arc_strong: Arc::strong_count(&cpu_throttled_tick),
                    update_github_token_strong: Arc::strong_count(&update_ctx_tick.github_token),
                    web_console_running: wc_tick.borrow().is_running(),
                    alarm_alert_alive,
                    visible_windows_mask,
                    audio: audio_clone.ref_snapshot(),
                });
            }
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
                    TriggerEvent::Alarm {
                        alarm_id,
                        label,
                        cycle_summary,
                        repeat,
                        play_chime_before,
                        custom_audio_path,
                        spoken_text,
                        triggered_once,
                        is_snooze,
                    } => {
                        win_sys::log_info(
                            "scheduler",
                            &format!(
                                "trigger alarm id={alarm_id} label={label} snooze={is_snooze} once={triggered_once} repeat={repeat} volume={volume_snapshot} chime_before={play_chime_before} custom={:?} tts={:?}",
                                custom_audio_path, spoken_text
                            ),
                        );

                        st.last_triggered_alarm = st
                            .config
                            .alarms
                            .iter()
                            .find(|a| a.id == alarm_id)
                            .cloned()
                            .or_else(|| {
                                Some(CustomAlarm {
                                    id: alarm_id,
                                    label: label.clone(),
                                    repeat_count: repeat,
                                    play_chime_before,
                                    custom_audio_path: custom_audio_path.clone().unwrap_or_default(),
                                    tts_enabled: spoken_text.is_some(),
                                    tts_text: spoken_text.clone().unwrap_or_default(),
                                    ..CustomAlarm::default()
                                })
                            });

                        audio_clone.play_alarm(
                            repeat,
                            volume_snapshot,
                            custom_audio_path.clone(),
                            play_chime_before,
                            spoken_text.clone(),
                            config_snapshot.tts_engine,
                            config_snapshot.edge_tts_voice.clone(),
                            config_snapshot.tts_rate,
                        );

                        if triggered_once {
                            let mut changed = false;
                            if let Some(a) = st.config.alarms.iter_mut().find(|a| a.id == alarm_id) {
                                if a.enabled {
                                    a.enabled = false;
                                    changed = true;
                                }
                            }
                            if changed {
                                let _ = st.config_store.save(&st.config);
                                wc_tick
                                    .borrow()
                                    .update_config_view(config_to_web_view(&st.config));
                                if let Some(da) = drafts_tick
                                    .borrow_mut()
                                    .iter_mut()
                                    .find(|a| a.id == alarm_id)
                                {
                                    da.enabled = false;
                                }
                                if let Some(sw) = sw_weak.upgrade() {
                                    sync_alarm_list_to_settings(&sw, &drafts_tick.borrow());
                                }
                            }
                        }

                        if let Some(aaw) = ensure_alarm_alert_window(
                            &aaw_slot_tick,
                            &st_rc,
                            &mw_weak,
                            &audio_clone,
                            config_snapshot.theme.to_index(),
                        ) {
                            let display_label = if is_snooze {
                                format!("{label}（稍后提醒）")
                            } else {
                                label
                            };
                            let spoken_preview = spoken_text.unwrap_or_else(|| {
                                if custom_audio_path.is_some() {
                                    "正在播放自定义闹钟音频...".to_string()
                                } else {
                                    "正在播放闹钟提示音...".to_string()
                                }
                            });
                            aaw.set_alarm_label(display_label.into());
                            aaw.set_alarm_time(now.format("%H:%M").to_string().into());
                            aaw.set_cycle_text(cycle_summary.into());
                            aaw.set_spoken_preview(spoken_preview.into());
                            center_and_show_window(&aaw, 390.0, 230.0);
                            bring_window_to_front(&aaw);
                        }
                    }
                }
            }

            let audio_busy = audio_clone.is_busy();
            let _busy_changed = audio_busy != prev_audio_busy;
            prev_audio_busy = audio_busy;

            let wake_now = ipc_wake_for_tick.swap(false, Ordering::SeqCst);
            if let Some(mw) = mw_weak.upgrade() {
                if wake_now {
                    refresh_main_window_ui(&mw, &st, audio_busy);
                    center_and_show_window(&mw, 460.0, 376.0);
                } else if mw.window().is_visible() {
                    // 仅当主状态窗口可见时刷新界面文本，最小化常驻托盘期间跳过字符串分配，保持零堆内存增长
                    refresh_main_window_ui(&mw, &st, audio_busy);
                }
            }

            if wc_tick.borrow().is_running() {
                let next = st.scheduler.compute_next_event(&st.config, now);
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
                    current_date: format!(
                        "{} · {}",
                        now.format("%Y-%m-%d"),
                        weekday_zh(now.weekday())
                    ),
                    mute_mode: st.config.mute_mode,
                    is_quiet_now,
                    is_audio_playing: audio_busy,
                    quiet_summary,
                    next_event_title: next.title,
                    next_event_time: next.target_time_text,
                    next_event_countdown: next.countdown_text,
                    chime_minutes: st.config.format_chime_minutes(),
                    enabled_alarms_count: st.config.alarms.iter().filter(|a| a.enabled).count(),
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
                        .describe_day_calendar(now.date_naive()),
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
    fn gc_decision_triggers_above_85mb_and_respects_cooldown() {
        let under = PerfSample {
            working_set_bytes: 40 * 1024 * 1024,
            private_bytes: 68 * 1024 * 1024,
            process_cpu_pct: 0.3,
            system_cpu_pct: 12.0,
            source: "test",
        };
        assert_eq!(
            evaluate_gc_decision(&under, None, MEMORY_GC_THRESHOLD_BYTES, GC_COOLDOWN),
            GcDecision::None
        );

        let over = PerfSample {
            working_set_bytes: 90 * 1024 * 1024,
            private_bytes: 92 * 1024 * 1024,
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
                Some(Duration::from_secs(305)),
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

