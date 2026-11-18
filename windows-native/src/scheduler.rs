use crate::audio::AudioRefSnapshot;
use crate::config::{
    render_alarm_tts_template, render_holiday_template, render_tts_template, AlarmRepeatMode,
    AppConfig, CustomAlarm,
};
use crate::win_sys;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Timelike};
use std::collections::{HashMap, VecDeque};

/// 2010–2040 紫金山标准农历位图压缩表（2010-02-14 为正月初一起点）
/// bit 0..=3: 闰月月份（0 表示无闰月）；bit 4..=15: 正月到腊月大小月（1=30天，0=29天）；bit 16: 闰月大小月
const LUNAR_START_YEAR: i32 = 2010;
const LUNAR_INFO: [u32; 31] = [
    0x0a950, 0x0b4a0, 0x0baa4, 0x0ad50, 0x055d9, // 2010–2014
    0x04ba0, 0x0a5b0, 0x15176, 0x052b0, 0x0a930, // 2015–2019
    0x07954, 0x06aa0, 0x0ad50, 0x05b52, 0x04b60, // 2020–2024
    0x0a6e6, 0x0a4e0, 0x0d260, 0x0ea65, 0x0d530, // 2025–2029
    0x05aa0, 0x076a3, 0x096d0, 0x04afb, 0x04ad0, // 2030–2034
    0x0a4d0, 0x1d0b6, 0x0d250, 0x0d520, 0x0dd45, // 2035–2039
    0x0b5a0, // 2040
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LunarDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub is_leap: bool,
}

impl LunarDate {
    pub fn format_zh(self) -> String {
        const MONTHS: [&str; 12] = [
            "正", "二", "三", "四", "五", "六", "七", "八", "九", "十", "冬", "腊",
        ];
        const DAYS: [&str; 30] = [
            "初一", "初二", "初三", "初四", "初五", "初六", "初七", "初八", "初九", "初十",
            "十一", "十二", "十三", "十四", "十五", "十六", "十七", "十八", "十九", "二十",
            "廿一", "廿二", "廿三", "廿四", "廿五", "廿六", "廿七", "廿八", "廿九", "三十",
        ];
        let m_idx = usize::from(self.month.saturating_sub(1)).min(11);
        let d_idx = usize::from(self.day.saturating_sub(1)).min(29);
        let leap_prefix = if self.is_leap { "闰" } else { "" };
        format!("农历{}{}月{}", leap_prefix, MONTHS[m_idx], DAYS[d_idx])
    }
}

#[inline]
fn lunar_leap_month(info: u32) -> u8 {
    (info & 0x0f) as u8
}

#[inline]
fn lunar_leap_days(info: u32) -> i64 {
    if lunar_leap_month(info) == 0 {
        0
    } else if (info & 0x10000) != 0 {
        30
    } else {
        29
    }
}

#[inline]
fn lunar_month_days(info: u32, month: u8) -> i64 {
    if (info & (0x10000 >> month)) != 0 {
        30
    } else {
        29
    }
}

fn lunar_year_days(info: u32) -> i64 {
    let mut sum: i64 = 348;
    let mut mask = 0x8000u32;
    while mask > 0x8 {
        if (info & mask) != 0 {
            sum += 1;
        }
        mask >>= 1;
    }
    sum + lunar_leap_days(info)
}

/// 将公历日期转换为精确农历日期（支持 2010–2040 年含闰月推算）
pub fn solar_to_lunar(date: NaiveDate) -> Option<LunarDate> {
    let base = NaiveDate::from_ymd_opt(2010, 2, 14)?;
    let mut offset = (date - base).num_days();
    if offset < 0 {
        return None;
    }

    let mut year_idx = None;
    for (idx, &info) in LUNAR_INFO.iter().enumerate() {
        let yd = lunar_year_days(info);
        if offset < yd {
            year_idx = Some(idx);
            break;
        }
        offset -= yd;
    }

    let idx = year_idx?;
    let info = LUNAR_INFO[idx];
    let year = LUNAR_START_YEAR + idx as i32;
    let lm = lunar_leap_month(info);

    for m in 1..=12u8 {
        let md = lunar_month_days(info, m);
        if offset < md {
            return Some(LunarDate {
                year,
                month: m,
                day: (offset + 1) as u8,
                is_leap: false,
            });
        }
        offset -= md;

        if m == lm {
            let ld = lunar_leap_days(info);
            if offset < ld {
                return Some(LunarDate {
                    year,
                    month: m,
                    day: (offset + 1) as u8,
                    is_leap: true,
                });
            }
            offset -= ld;
        }
    }

    None
}

/// 节假日分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolidayKind {
    Statutory,
    Traditional,
}

/// 匹配到的内置节假日及问候信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolidayInfo {
    pub name: String,
    pub kind: HolidayKind,
    pub greeting: String,
}

/// 计算 21 世纪清明节公历日期（2000–2099 年 4 月日序公式）
fn is_qingming(date: NaiveDate) -> bool {
    if date.month() != 4 {
        return false;
    }
    let year = date.year();
    if !(2000..=2099).contains(&year) {
        return date.day() == 4 || date.day() == 5;
    }
    let y = (year % 100) as f64;
    let qm_day = (y * 0.2422 + 4.81).floor() as u32 - ((year % 100) as u32 / 4);
    date.day() == qm_day
}

/// 内置中国法定与传统公历/农历节假日识别（纯本地离线计算，无需外部接口）
pub fn lookup_builtin_holiday(date: NaiveDate, lunar: Option<LunarDate>) -> Option<HolidayInfo> {
    // 1. 优先判断除夕（明天为农历正月初一，兼容大年廿九与大年三十）
    if let Some(tomorrow) = date.checked_add_days(chrono::Days::new(1)) {
        if let Some(tl) = solar_to_lunar(tomorrow) {
            if !tl.is_leap && tl.month == 1 && tl.day == 1 {
                return Some(HolidayInfo {
                    name: "除夕".to_string(),
                    kind: HolidayKind::Statutory,
                    greeting: "除夕守岁迎新春，愿您阖家团圆、辞旧迎新！".to_string(),
                });
            }
        }
    }

    // 2. 传统农历节日
    if let Some(l) = lunar {
        if !l.is_leap {
            let hit = match (l.month, l.day) {
                (1, 1) => Some((
                    "春节",
                    HolidayKind::Statutory,
                    "新春大吉，祝您新年快乐、万事顺意！",
                )),
                (1, 15) => Some((
                    "元宵节",
                    HolidayKind::Traditional,
                    "元宵佳节喜团圆，祝您阖家欢乐、幸福安康！",
                )),
                (5, 5) => Some((
                    "端午节",
                    HolidayKind::Statutory,
                    "端午安康，祝您顺遂安好、幸福常伴！",
                )),
                (7, 7) => Some((
                    "七夕节",
                    HolidayKind::Traditional,
                    "七夕快乐，愿美好与幸运常伴您左右！",
                )),
                (8, 15) => Some((
                    "中秋节",
                    HolidayKind::Statutory,
                    "中秋花好月圆，祝您人月两圆、万事如意！",
                )),
                (9, 9) => Some((
                    "重阳节",
                    HolidayKind::Traditional,
                    "九九重阳敬长辈，祝愿阖家安康、福寿绵长！",
                )),
                (12, 8) => Some((
                    "腊八节",
                    HolidayKind::Traditional,
                    "过了腊八就是年，天气转凉请注意添衣保暖！",
                )),
                _ => None,
            };
            if let Some((name, kind, greeting)) = hit {
                return Some(HolidayInfo {
                    name: name.to_string(),
                    kind,
                    greeting: greeting.to_string(),
                });
            }
        }
    }

    // 3. 公历法定节假日与清明节
    if is_qingming(date) {
        return Some(HolidayInfo {
            name: "清明节".to_string(),
            kind: HolidayKind::Statutory,
            greeting: "清明春和景明，外出踏青请注意安全！".to_string(),
        });
    }

    let solar_hit = match (date.month(), date.day()) {
        (1, 1) => Some((
            "元旦",
            HolidayKind::Statutory,
            "新年新气象，元旦快乐，祝您新的一年诸事顺遂！",
        )),
        (5, 1) => Some((
            "劳动节",
            HolidayKind::Statutory,
            "五一劳动节快乐，忙碌之余记得劳逸结合、好好休息！",
        )),
        (10, 1) => Some((
            "国庆节",
            HolidayKind::Statutory,
            "欢度国庆佳节，祝您假期愉快、国泰民安！",
        )),
        _ => None,
    };

    solar_hit.map(|(name, kind, greeting)| HolidayInfo {
        name: name.to_string(),
        kind,
        greeting: greeting.to_string(),
    })
}

/// 触发事件类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerEvent {
    Chime {
        repeat: u8,
        /// 开启语音报时或节假日模式时生成的真实时间/节庆问候播报文本
        spoken_time_text: Option<String>,
        /// 节假日专属报时音路径（仅当命中节假日且配置了专属音频时生效）
        holiday_audio_path: Option<String>,
        /// 命中的节假日名称（便于日志与状态追踪）
        holiday_name: Option<String>,
    },
    Alarm {
        alarm_id: u64,
        label: String,
        cycle_summary: String,
        repeat: u8,
        play_chime_before: bool,
        custom_audio_path: Option<String>,
        spoken_text: Option<String>,
        /// 是否为“仅一次”闹钟触发（调用方响铃后自动关闭该闹钟）
        triggered_once: bool,
        /// 是否来自“稍后提醒”
        is_snooze: bool,
    },
}

/// 主界面倒计时摘要信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEventInfo {
    pub title: String,
    pub countdown_text: String,
    pub target_time_text: String,
}

/// 调度器内存泄漏周期监测采样间隔（秒）：每 60 秒为一个独立观测周期
const LEAK_CYCLE_INTERVAL_SECS: i64 = 60;
/// 判定为非预期单调递增所需的连续上升周期数（baseline + 5 个连续上升周期共 6 个采样点）
const LEAK_CONSECUTIVE_CYCLES: usize = 5;
/// 单周期视为真实增长的最小增量阈值（16 KiB，过滤 OS 页表与小对象复用波动）
const LEAK_MIN_STEP_BYTES: u64 = 16 * 1024;
/// 连续 5 个周期累计净增长告警阈值（256 KiB）
const LEAK_MIN_TOTAL_GROWTH_BYTES: u64 = 256 * 1024;

/// 主循环传给调度器的运行时对象引用计数与活跃资源快照
#[derive(Debug, Clone, Default)]
pub struct ScheduledRefContext {
    pub app_state_strong: usize,
    pub draft_alarms_strong: usize,
    pub web_console_strong: usize,
    pub alarm_alert_slot_strong: usize,
    pub single_instance_guard_strong: usize,
    pub voice_preview_gen_strong: usize,
    pub tray_arc_strong: usize,
    pub ipc_wake_arc_strong: usize,
    pub perf_sample_arc_strong: usize,
    pub cpu_throttled_arc_strong: usize,
    pub update_github_token_strong: usize,
    pub web_console_running: bool,
    pub alarm_alert_alive: bool,
    /// 可见窗口位掩码：bit0=Main bit1=Settings bit2=About bit3=Update bit4=AlarmAlert
    pub visible_windows_mask: u8,
    pub audio: AudioRefSnapshot,
}

#[derive(Debug, Clone)]
struct MemoryCycleSample {
    cycle_bucket: i64,
    timestamp_text: String,
    working_set_bytes: u64,
    private_bytes: u64,
    tracked_bytes: u64,
}

/// 连续 5 个调度周期内存单调递增泄漏监控器
#[derive(Debug, Default)]
struct MemoryLeakMonitor {
    last_cycle_bucket: Option<i64>,
    last_alarm_alert_alive: bool,
    samples: VecDeque<MemoryCycleSample>,
    alert_count: u64,
}

/// 核心定时调度器（负责整/半点及自定义闹钟节拍识别、防重入、跨日静默判定、内置节假日、倒计时与周期泄漏监控）
#[derive(Debug, Default)]
pub struct Scheduler {
    last_chime_slot: Option<(i32, u32, u32, u32)>,
    last_alarm_slots: HashMap<u64, (i32, u32, u32, u32)>,
    snooze_alarm: Option<(CustomAlarm, DateTime<Local>)>,
    leak_monitor: MemoryLeakMonitor,
    runtime_refs: ScheduledRefContext,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 判断当前时刻是否进入新的 60 秒内存观测周期
    pub fn needs_leak_sample_at(&self, now: DateTime<Local>) -> bool {
        let bucket = now.timestamp().div_euclid(LEAK_CYCLE_INTERVAL_SECS);
        self.leak_monitor.last_cycle_bucket != Some(bucket)
    }

    /// 更新主循环提供的 Rc/Arc 强引用计数与活跃资源快照
    pub fn set_runtime_refs(&mut self, refs: ScheduledRefContext) {
        self.runtime_refs = refs;
    }

    /// 在每个 60 秒调度周期检测进程内存是否连续 5 个周期呈非预期单调递增；
    /// 若命中则记录完整增长曲线、Scheduler 容器状态及各子系统对象引用计数到日志。
    pub fn check_leak_at_cycle(
        &mut self,
        cfg: &AppConfig,
        now: DateTime<Local>,
        working_set_bytes: u64,
        private_bytes: u64,
    ) -> Option<String> {
        let cycle_bucket = now.timestamp().div_euclid(LEAK_CYCLE_INTERVAL_SECS);
        if self.leak_monitor.last_cycle_bucket == Some(cycle_bucket) {
            return None;
        }
        self.leak_monitor.last_cycle_bucket = Some(cycle_bucket);

        let tracked_bytes = if private_bytes > 0 {
            private_bytes
        } else {
            working_set_bytes
        };
        if tracked_bytes == 0 {
            return None;
        }

        let current_sample = MemoryCycleSample {
            cycle_bucket,
            timestamp_text: now.format("%H:%M:%S").to_string(),
            working_set_bytes,
            private_bytes,
            tracked_bytes,
        };

        // 懒加载闹钟弹窗首次实例化会带来一次性 Slint 纹理/组件内存抬升，重置基线避免误报
        let alert_just_created =
            self.runtime_refs.alarm_alert_alive && !self.leak_monitor.last_alarm_alert_alive;
        self.leak_monitor.last_alarm_alert_alive = self.runtime_refs.alarm_alert_alive;
        if alert_just_created {
            self.leak_monitor.samples.clear();
            self.leak_monitor.samples.push_back(current_sample);
            return None;
        }

        // 若正在播放音频/TTS 语音，临时解码缓冲会短暂抬高占用，跳过本周期防止误计入单调序列
        if self.runtime_refs.audio.active_seq_gen != 0
            || self.runtime_refs.audio.active_sink_present
        {
            return None;
        }

        let Some(prev) = self.leak_monitor.samples.back() else {
            self.leak_monitor.samples.push_back(current_sample);
            return None;
        };

        if current_sample.tracked_bytes < prev.tracked_bytes.saturating_add(LEAK_MIN_STEP_BYTES) {
            self.leak_monitor.samples.clear();
            self.leak_monitor.samples.push_back(current_sample);
            return None;
        }

        self.leak_monitor.samples.push_back(current_sample);
        while self.leak_monitor.samples.len() > LEAK_CONSECUTIVE_CYCLES + 1 {
            self.leak_monitor.samples.pop_front();
        }

        if self.leak_monitor.samples.len() < LEAK_CONSECUTIVE_CYCLES + 1 {
            return None;
        }

        let first = self.leak_monitor.samples.front()?;
        let last = self.leak_monitor.samples.back()?;
        let net_growth = last.tracked_bytes.saturating_sub(first.tracked_bytes);
        if net_growth < LEAK_MIN_TOTAL_GROWTH_BYTES {
            return None;
        }

        self.leak_monitor.alert_count = self.leak_monitor.alert_count.saturating_add(1);
        let curve = self
            .leak_monitor
            .samples
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let delta_kb = if idx == 0 {
                    0.0
                } else {
                    let prev_b = self.leak_monitor.samples[idx - 1].tracked_bytes;
                    (s.tracked_bytes.saturating_sub(prev_b) as f64) / 1024.0
                };
                format!(
                    "{}@{}(priv={:.2}MB,ws={:.2}MB,+{:.1}KB)",
                    s.cycle_bucket,
                    s.timestamp_text,
                    s.private_bytes as f64 / (1024.0 * 1024.0),
                    s.working_set_bytes as f64 / (1024.0 * 1024.0),
                    delta_kb
                )
            })
            .collect::<Vec<_>>()
            .join(" -> ");

        let r = &self.runtime_refs;
        let a = &r.audio;
        let report = format!(
            "monotonic memory growth detected across {} consecutive scheduled cycles (#{}, net=+{:.2}MB / {}B): curve=[{}] | \
            scheduler={{last_chime_slot={}, last_alarm_slots={}/{}, snooze_active={}, cfg_alarms={}, custom_chime_mins={}}} | \
            rc/arc={{app_state={}, draft_alarms={}, web_console={}, alarm_alert_slot={}, single_instance={}, voice_preview_gen={}, \
            tray={}, ipc_wake={}, perf_sample={}, cpu_throttled={}, update_token={}, web_running={}, alert_alive={}, visible_windows=0x{:02x}}} | \
            audio_engine_refs={{gen_counter={}, active_seq_gen_arc={}, live_volume={}, tts_cache={}, buffer_pool={}, tts_status_tx={}, \
            active_sink={}, active_seq_gen={}}} | \
            pool={{byte_bufs={}, byte_cap={}B, byte_hits={}, byte_misses={}, sample_queues={}, sample_cap={}, sample_hits={}, sample_misses={}}} | \
            tts_cache={{entries={}, bytes={}B}}",
            LEAK_CONSECUTIVE_CYCLES,
            self.leak_monitor.alert_count,
            net_growth as f64 / (1024.0 * 1024.0),
            net_growth,
            curve,
            self.last_chime_slot.is_some(),
            self.last_alarm_slots.len(),
            self.last_alarm_slots.capacity(),
            self.snooze_alarm.is_some(),
            cfg.alarms.len(),
            cfg.custom_chime_minutes.len(),
            r.app_state_strong,
            r.draft_alarms_strong,
            r.web_console_strong,
            r.alarm_alert_slot_strong,
            r.single_instance_guard_strong,
            r.voice_preview_gen_strong,
            r.tray_arc_strong,
            r.ipc_wake_arc_strong,
            r.perf_sample_arc_strong,
            r.cpu_throttled_arc_strong,
            r.update_github_token_strong,
            r.web_console_running,
            r.alarm_alert_alive,
            r.visible_windows_mask,
            a.gen_counter_strong,
            a.active_seq_gen_strong,
            a.live_volume_strong,
            a.tts_cache_strong,
            a.buffer_pool_strong,
            a.tts_status_tx_strong,
            a.active_sink_present,
            a.active_seq_gen,
            a.pool_byte_buffers,
            a.pool_byte_capacity,
            a.pool_byte_hits,
            a.pool_byte_misses,
            a.pool_sample_queues,
            a.pool_sample_capacity,
            a.pool_sample_hits,
            a.pool_sample_misses,
            a.tts_cache_entries,
            a.tts_cache_bytes,
        );

        win_sys::log_warn("leak-watch", &report);
        win_sys::flush_logs();

        // 告警输出后以最新采样点重建基线，避免连续刷屏，若后续再连续 5 个周期上升则再次捕获
        if let Some(latest) = self.leak_monitor.samples.pop_back() {
            self.leak_monitor.samples.clear();
            self.leak_monitor.samples.push_back(latest);
        }

        Some(report)
    }

    /// 校时后重置防重入槽位，防止系统时间回调或向前校时后错过提醒
    pub fn reset_slots(&mut self) {
        self.last_chime_slot = None;
        self.last_alarm_slots.clear();
    }

    /// 保存配置后按变更幅度同步防重入槽位：
    /// 保留本分钟已播报的整/半点报时槽位以及触发时间(hour, minute)未变的闹钟槽位，
    /// 避免在刚报时/响铃的同一分钟内保存其他设置后再次重复触发；若闹钟调整了新时间则清理旧槽位。
    pub fn sync_slots_after_config_save(&mut self, prev_cfg: &AppConfig, next_cfg: &AppConfig) {
        self.last_alarm_slots.retain(|alarm_id, _| {
            let prev_time = prev_cfg
                .alarms
                .iter()
                .find(|a| a.id == *alarm_id)
                .map(|a| (a.hour, a.minute));
            let next_time = next_cfg
                .alarms
                .iter()
                .find(|a| a.id == *alarm_id)
                .map(|a| (a.hour, a.minute));
            matches!((prev_time, next_time), (Some(pt), Some(nt)) if pt == nt)
        });
    }

    pub fn schedule_snooze(&mut self, alarm: CustomAlarm, trigger_at: DateTime<Local>) {
        self.snooze_alarm = Some((alarm, trigger_at));
    }

    pub fn clear_snooze(&mut self) {
        self.snooze_alarm = None;
    }

    /// 查询指定公历日期命中的内置公历/农历节假日
    pub fn lookup_holiday(&self, date: NaiveDate) -> Option<HolidayInfo> {
        let lunar = solar_to_lunar(date);
        lookup_builtin_holiday(date, lunar)
    }

    /// 生成当天历法摘要（供主窗日期栏及设置页“今日历法预览”使用）
    pub fn describe_day_calendar(&self, date: NaiveDate) -> String {
        let lunar_text = solar_to_lunar(date)
            .map(|l| l.format_zh())
            .unwrap_or_else(|| "农历数据外年份".to_string());
        if let Some(h) = self.lookup_holiday(date) {
            format!("{lunar_text} · 节日：{name}", name = h.name)
        } else {
            let is_weekend = date.weekday().num_days_from_monday() >= 5;
            if is_weekend {
                format!("{lunar_text} · 周末")
            } else {
                format!("{lunar_text} · 工作日")
            }
        }
    }

    /// 根据用户开关与当前系统时间生成半/整点真实时间播报文本
    pub fn build_chime_announcement(
        tts_enabled: bool,
        template: &str,
        now: DateTime<Local>,
    ) -> Option<String> {
        if !tts_enabled {
            return None;
        }
        Some(render_tts_template(template, now.hour(), now.minute()))
    }

    /// 结合农历/节假日模式与普通语音开关生成报时 TTS 播报文本
    pub fn build_chime_announcement_with_holiday(
        cfg: &AppConfig,
        now: DateTime<Local>,
        holiday: Option<&HolidayInfo>,
    ) -> Option<String> {
        if cfg.holiday_enabled {
            if let Some(h) = holiday {
                let lunar_label = solar_to_lunar(now.date_naive())
                    .map(|l| l.format_zh())
                    .unwrap_or_default();
                let rendered = render_holiday_template(
                    &cfg.holiday_greeting_template,
                    &h.name,
                    &h.greeting,
                    &lunar_label,
                    now.hour(),
                    now.minute(),
                );
                if !rendered.trim().is_empty() {
                    return Some(rendered);
                }
            }
        }
        Self::build_chime_announcement(cfg.chime_tts_enabled, &cfg.chime_tts_template, now)
    }

    /// 根据闹钟配置与当前系统时间生成自定义闹钟文字转语音文本
    pub fn build_alarm_announcement(alarm: &CustomAlarm, now: DateTime<Local>) -> Option<String> {
        if !alarm.tts_enabled {
            return None;
        }
        let trimmed = alarm.tts_text.trim();
        let tpl = if trimmed.is_empty() {
            "现在时间，{TIME}，{LABEL}"
        } else {
            trimmed
        };
        let rendered = render_alarm_tts_template(tpl, &alarm.label, now.hour(), now.minute());
        if rendered.trim().is_empty() {
            None
        } else {
            Some(rendered)
        }
    }

    /// 判断给定 (hour, minute) 是否位于配置的静默时段内
    pub fn is_quiet_at(cfg: &AppConfig, hour: u32, minute: u32) -> bool {
        if !cfg.quiet_enabled {
            return false;
        }
        let current = hour * 60 + minute;
        let start = cfg.quiet_start_hour as u32 * 60 + cfg.quiet_start_minute as u32;
        let end = cfg.quiet_end_hour as u32 * 60 + cfg.quiet_end_minute as u32;

        if start < end {
            current >= start && current < end
        } else if start > end {
            current >= start || current < end
        } else {
            false
        }
    }

    /// 栈上构建有效报时分钟位图，避免每 250ms 节拍重复在堆上分配 Vec<u8>
    #[inline]
    fn chime_minute_mask(cfg: &AppConfig) -> ([bool; 60], bool) {
        let mut mask = [false; 60];
        let mut any = false;
        if cfg.hourly_chime_enabled {
            mask[0] = true;
            any = true;
        }
        if cfg.half_hour_chime_enabled {
            mask[30] = true;
            any = true;
        }
        for &m in &cfg.custom_chime_minutes {
            if m <= 59 {
                mask[usize::from(m)] = true;
                any = true;
            }
        }
        (mask, any)
    }

    /// 周期检测（建议每 250ms–500ms 调用一次），返回本拍需要触发的事件列表
    pub fn evaluate_at(&mut self, cfg: &AppConfig, now: DateTime<Local>) -> Vec<TriggerEvent> {
        let mut events = Vec::new();
        let slot = (now.year(), now.ordinal(), now.hour(), now.minute());
        let minute_u8 = now.minute() as u8;
        let hour_u8 = now.hour() as u8;
        let today = now.date_naive();

        // 1. 报时检测（由整点、半点及可选额外分钟勾选控制，受静音模式与静默时段约束）
        let (minute_mask, any_chime) = Self::chime_minute_mask(cfg);
        if any_chime
            && minute_mask[usize::from(minute_u8.min(59))]
            && self.last_chime_slot != Some(slot)
        {
            self.last_chime_slot = Some(slot);
            if !cfg.mute_mode && !Self::is_quiet_at(cfg, now.hour(), now.minute()) {
                let holiday = if cfg.holiday_enabled {
                    self.lookup_holiday(today)
                } else {
                    None
                };

                let spoken_time_text =
                    Self::build_chime_announcement_with_holiday(cfg, now, holiday.as_ref());

                let holiday_audio_path = if cfg.holiday_enabled && holiday.is_some() {
                    let p = cfg.holiday_custom_audio_path.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                } else {
                    None
                };

                events.push(TriggerEvent::Chime {
                    repeat: cfg.chime_repeat_count,
                    spoken_time_text,
                    holiday_audio_path,
                    holiday_name: holiday.map(|h| h.name),
                });
            }
        }

        // 2. 稍后提醒（Snooze）检测
        if let Some((snooze_alarm, trigger_at)) = &self.snooze_alarm {
            if now >= *trigger_at {
                let alarm = snooze_alarm.clone();
                self.snooze_alarm = None;
                if !cfg.mute_mode {
                    let spoken_text = Self::build_alarm_announcement(&alarm, now);
                    let custom_audio_path = {
                        let p = alarm.custom_audio_path.trim();
                        if p.is_empty() {
                            None
                        } else {
                            Some(p.to_string())
                        }
                    };
                    events.push(TriggerEvent::Alarm {
                        alarm_id: alarm.id,
                        label: alarm.label.clone(),
                        cycle_summary: "稍后提醒".to_string(),
                        repeat: alarm.repeat_count,
                        play_chime_before: alarm.play_chime_before,
                        custom_audio_path,
                        spoken_text,
                        triggered_once: false,
                        is_snooze: true,
                    });
                }
            }
        }

        // 3. 自定义闹钟检测（支持多种周期：仅一次/每天/工作日/周末/自定义星期/每隔N天）
        for alarm in &cfg.alarms {
            if !alarm.enabled {
                continue;
            }
            if hour_u8 != alarm.hour || minute_u8 != alarm.minute {
                continue;
            }
            if !alarm.should_ring_on_date(today) {
                continue;
            }
            if self.last_alarm_slots.get(&alarm.id) == Some(&slot) {
                continue;
            }
            self.last_alarm_slots.insert(alarm.id, slot);

            if cfg.mute_mode {
                continue;
            }
            if alarm.respect_quiet_hours && Self::is_quiet_at(cfg, now.hour(), now.minute()) {
                continue;
            }

            let spoken_text = Self::build_alarm_announcement(alarm, now);
            let custom_audio_path = {
                let p = alarm.custom_audio_path.trim();
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                }
            };

            events.push(TriggerEvent::Alarm {
                alarm_id: alarm.id,
                label: alarm.label.clone(),
                cycle_summary: alarm.format_cycle_summary(),
                repeat: alarm.repeat_count,
                play_chime_before: alarm.play_chime_before,
                custom_audio_path,
                spoken_text,
                triggered_once: alarm.repeat_mode == AlarmRepeatMode::Once,
                is_snooze: false,
            });
        }

        // 清理跨日或已删除闹钟的过期防重入槽位，防止 HashMap 在长期驻留期间累积历史记录
        if !self.last_alarm_slots.is_empty() {
            self.last_alarm_slots.retain(|id, recorded| {
                recorded.0 == slot.0
                    && recorded.1 == slot.1
                    && cfg.alarms.iter().any(|a| a.id == *id)
            });
        }

        // 4. 调度周期内存泄漏监控（每 60s 观测一次，连续 5 个周期非预期单调递增时输出详细对象引用快照）
        if self.needs_leak_sample_at(now) {
            let (ws_bytes, private_bytes) = win_sys::query_process_memory_bytes();
            let _ = self.check_leak_at_cycle(cfg, now, ws_bytes, private_bytes);
        }

        events
    }

    /// 查找指定闹钟在 `now` 之后的下一次有效响铃时刻
    pub fn next_alarm_trigger(
        cfg: &AppConfig,
        alarm: &CustomAlarm,
        now: DateTime<Local>,
    ) -> Option<DateTime<Local>> {
        if !alarm.enabled {
            return None;
        }
        let today = now.date_naive();
        let max_days = if alarm.repeat_mode == AlarmRepeatMode::IntervalDays {
            i64::from(alarm.interval_days.max(1)) + 2
        } else {
            366
        };

        for day_offset in 0..=max_days {
            let date = today + Duration::days(day_offset);
            if !alarm.should_ring_on_date(date) {
                continue;
            }
            if alarm.respect_quiet_hours
                && Self::is_quiet_at(cfg, u32::from(alarm.hour), u32::from(alarm.minute))
            {
                continue;
            }
            if let Some(naive) = date.and_hms_opt(u32::from(alarm.hour), u32::from(alarm.minute), 0)
            {
                if let Some(dt) = naive.and_local_timezone(Local).single() {
                    if dt > now {
                        return Some(dt);
                    }
                }
            }
        }
        None
    }

    /// 计算距离下一个“整/半点报时”或“自定义闹钟”的精确倒计时
    pub fn compute_next_event(&self, cfg: &AppConfig, now: DateTime<Local>) -> NextEventInfo {
        enum WinnerKind {
            Chime(&'static str),
            Snooze,
            Alarm(usize),
        }

        let mut best: Option<(DateTime<Local>, WinnerKind)> = None;
        let mut consider = |dt: DateTime<Local>, kind: WinnerKind| {
            if best.as_ref().is_none_or(|(cur_dt, _)| dt < *cur_dt) {
                best = Some((dt, kind));
            }
        };

        let (minute_mask, any_chime) = Self::chime_minute_mask(cfg);

        // 1. 向前搜索最多 48 小时内的下一个有效报时时刻（自动跳过静默时段）
        if any_chime {
            let base = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now)
                + Duration::minutes(1);

            for offset_min in 0..(48 * 60) {
                let candidate = base + Duration::minutes(offset_min);
                let m = candidate.minute() as u8;
                if minute_mask[usize::from(m.min(59))]
                    && !Self::is_quiet_at(cfg, candidate.hour(), candidate.minute())
                {
                    let label = if m == 0 {
                        "整点报时"
                    } else if m == 30 {
                        "半点报时"
                    } else {
                        "定时报时"
                    };
                    consider(candidate, WinnerKind::Chime(label));
                    break;
                }
            }
        }

        // 2. 稍后提醒（Snooze）
        if let Some((_, dt)) = &self.snooze_alarm {
            if *dt > now {
                consider(*dt, WinnerKind::Snooze);
            }
        }

        // 3. 所有启用中的自定义闹钟
        for (idx, alarm) in cfg.alarms.iter().enumerate() {
            if let Some(dt) = Self::next_alarm_trigger(cfg, alarm, now) {
                consider(dt, WinnerKind::Alarm(idx));
            }
        }

        if let Some((target, kind)) = best {
            let label = match kind {
                WinnerKind::Chime(s) => s.to_string(),
                WinnerKind::Snooze => match &self.snooze_alarm {
                    Some((snooze_alarm, _)) => format!("稍后提醒 · {}", snooze_alarm.label),
                    None => "稍后提醒".to_string(),
                },
                WinnerKind::Alarm(idx) => format!("闹钟 · {}", cfg.alarms[idx].label),
            };
            let secs = (target - now).num_seconds().max(0);
            let days = secs / 86_400;
            let hours = (secs % 86_400) / 3600;
            let mins = (secs % 3600) / 60;
            let rem_secs = secs % 60;
            let countdown_text = if days > 0 {
                format!("{days}天 {hours:02}:{mins:02}")
            } else if hours > 0 {
                format!("{hours:02}:{mins:02}:{rem_secs:02}")
            } else {
                format!("{mins:02}分{rem_secs:02}秒")
            };

            let today = now.date_naive();
            let target_date = target.date_naive();
            let target_time_text = if target_date == today {
                target.format("%H:%M").to_string()
            } else if target_date == today + Duration::days(1) {
                format!("明天 {}", target.format("%H:%M"))
            } else {
                format!("{} {}", target.format("%m-%d"), target.format("%H:%M"))
            };

            NextEventInfo {
                title: label,
                countdown_text,
                target_time_text,
            }
        } else {
            NextEventInfo {
                title: "报时与闹钟均未启用".to_string(),
                countdown_text: "--:--".to_string(),
                target_time_text: "--:--".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_cross_midnight_quiet_period() {
        let mut cfg = AppConfig::default();
        cfg.quiet_enabled = true;
        cfg.quiet_start_hour = 23;
        cfg.quiet_start_minute = 0;
        cfg.quiet_end_hour = 7;
        cfg.quiet_end_minute = 0;

        assert!(Scheduler::is_quiet_at(&cfg, 23, 0));
        assert!(Scheduler::is_quiet_at(&cfg, 23, 59));
        assert!(Scheduler::is_quiet_at(&cfg, 0, 0));
        assert!(Scheduler::is_quiet_at(&cfg, 6, 59));
        assert!(!Scheduler::is_quiet_at(&cfg, 7, 0));
        assert!(!Scheduler::is_quiet_at(&cfg, 22, 59));
    }

    #[test]
    fn test_chime_checkboxes_and_alarm_triggers() {
        let mut cfg = AppConfig::default();
        cfg.hourly_chime_enabled = true;
        cfg.half_hour_chime_enabled = false;
        cfg.alarms.push(CustomAlarm {
            id: 1,
            label: "喝水休息".to_string(),
            enabled: true,
            hour: 12,
            minute: 0,
            repeat_mode: AlarmRepeatMode::Daily,
            repeat_count: 2,
            play_chime_before: true,
            tts_enabled: true,
            tts_text: "现在时间，{TIME}，{LABEL}".to_string(),
            ..CustomAlarm::default()
        });

        let mut sched = Scheduler::new();

        // 半点已取消勾选，不应触发报时
        let t_half = Local.with_ymd_and_hms(2026, 11, 13, 11, 30, 0).unwrap();
        assert!(sched.evaluate_at(&cfg, t_half).is_empty());

        // 整点 + 12:00 闹钟同时触发
        let t_noon = Local.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let events = sched.evaluate_at(&cfg, t_noon);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], TriggerEvent::Chime { .. }));
        match &events[1] {
            TriggerEvent::Alarm {
                alarm_id,
                label,
                repeat,
                spoken_text,
                triggered_once,
                ..
            } => {
                assert_eq!(*alarm_id, 1);
                assert_eq!(label, "喝水休息");
                assert_eq!(*repeat, 2);
                assert_eq!(
                    spoken_text.as_deref(),
                    Some("现在时间，十二点整，喝水休息")
                );
                assert!(!triggered_once);
            }
            _ => panic!("expected alarm event"),
        }

        // 同一分钟内防重入
        let t_noon_next_sec = Local.with_ymd_and_hms(2026, 11, 13, 12, 0, 1).unwrap();
        assert!(sched.evaluate_at(&cfg, t_noon_next_sec).is_empty());
    }

    #[test]
    fn test_alarm_repeat_cycles_workdays_weekly_interval_once() {
        // 2026-11-16 是星期一
        let mon = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let wed = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        let sat = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();

        let workday_alarm = CustomAlarm {
            repeat_mode: AlarmRepeatMode::Workdays,
            ..CustomAlarm::default()
        };
        assert!(workday_alarm.should_ring_on_date(mon));
        assert!(workday_alarm.should_ring_on_date(wed));
        assert!(!workday_alarm.should_ring_on_date(sat));

        let weekend_alarm = CustomAlarm {
            repeat_mode: AlarmRepeatMode::Weekends,
            ..CustomAlarm::default()
        };
        assert!(!weekend_alarm.should_ring_on_date(mon));
        assert!(weekend_alarm.should_ring_on_date(sat));

        let weekly_alarm = CustomAlarm {
            repeat_mode: AlarmRepeatMode::Weekly,
            weekdays: [false, false, true, false, false, false, false], // 仅周三
            ..CustomAlarm::default()
        };
        assert!(!weekly_alarm.should_ring_on_date(mon));
        assert!(weekly_alarm.should_ring_on_date(wed));

        let interval_alarm = CustomAlarm {
            repeat_mode: AlarmRepeatMode::IntervalDays,
            interval_days: 2,
            anchor_date: "2026-11-16".to_string(),
            ..CustomAlarm::default()
        };
        assert!(interval_alarm.should_ring_on_date(mon));
        assert!(!interval_alarm.should_ring_on_date(NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()));
        assert!(interval_alarm.should_ring_on_date(wed));

        let once_alarm = CustomAlarm {
            repeat_mode: AlarmRepeatMode::Once,
            once_date: "2026-11-18".to_string(),
            ..CustomAlarm::default()
        };
        assert!(!once_alarm.should_ring_on_date(mon));
        assert!(once_alarm.should_ring_on_date(wed));
    }

    #[test]
    fn test_lunar_conversion_and_builtin_holidays() {
        // 2025 闰六月初一 -> 2025-07-25
        let leap_6_1 = solar_to_lunar(NaiveDate::from_ymd_opt(2025, 7, 25).unwrap()).unwrap();
        assert_eq!(leap_6_1.format_zh(), "农历闰六月初一");

        // 2025 中秋节（2025-10-06 为农历八月十五）
        let mid_autumn_2025 = NaiveDate::from_ymd_opt(2025, 10, 6).unwrap();
        let l_ma = solar_to_lunar(mid_autumn_2025).unwrap();
        let h_ma = lookup_builtin_holiday(mid_autumn_2025, Some(l_ma)).unwrap();
        assert_eq!(h_ma.name, "中秋节");

        // 2026 除夕与春节
        let eve_2026 = NaiveDate::from_ymd_opt(2026, 2, 16).unwrap();
        let cny_2026 = NaiveDate::from_ymd_opt(2026, 2, 17).unwrap();
        assert_eq!(
            lookup_builtin_holiday(eve_2026, solar_to_lunar(eve_2026))
                .unwrap()
                .name,
            "除夕"
        );
        assert_eq!(
            lookup_builtin_holiday(cny_2026, solar_to_lunar(cny_2026))
                .unwrap()
                .name,
            "春节"
        );

        // 2026 清明节（2026-04-05）
        let qm_2026 = NaiveDate::from_ymd_opt(2026, 4, 5).unwrap();
        assert_eq!(
            lookup_builtin_holiday(qm_2026, solar_to_lunar(qm_2026))
                .unwrap()
                .name,
            "清明节"
        );
    }

    #[test]
    fn test_holiday_chime_uses_builtin_offline_calendar() {
        let mut sched = Scheduler::new();
        let mut cfg = AppConfig::default();
        cfg.holiday_enabled = true;
        cfg.holiday_custom_audio_path = "D:\\audio\\fest.wav".to_string();
        cfg.holiday_greeting_template =
            "今天是{HOLIDAY}（{LUNAR}），{GREETING}现在时间，{TIME}".to_string();

        // 2026-10-01 国庆节
        let now = Local.with_ymd_and_hms(2026, 10, 1, 10, 0, 0).unwrap();
        let events = sched.evaluate_at(&cfg, now);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TriggerEvent::Chime {
                holiday_name,
                holiday_audio_path,
                spoken_time_text,
                ..
            } => {
                assert_eq!(holiday_name.as_deref(), Some("国庆节"));
                assert_eq!(holiday_audio_path.as_deref(), Some("D:\\audio\\fest.wav"));
                let text = spoken_time_text.as_deref().unwrap();
                assert!(text.contains("国庆节"));
                assert!(text.contains("十点整"));
            }
            _ => panic!("expected Chime event"),
        }
    }

    #[test]
    fn test_sync_slots_after_config_save_prevents_same_minute_retrigger() {
        let mut sched = Scheduler::new();
        let mut prev_cfg = AppConfig::default();
        prev_cfg.half_hour_chime_enabled = true;
        prev_cfg.alarms = vec![CustomAlarm {
            id: 1,
            enabled: true,
            label: "喝水提醒".to_string(),
            hour: 18,
            minute: 30,
            repeat_mode: AlarmRepeatMode::Daily,
            ..CustomAlarm::default()
        }];

        // 18:30:00 触发一次整半点报时 + 一次 18:30 闹钟
        let t0 = Local.with_ymd_and_hms(2026, 11, 18, 18, 30, 0).unwrap();
        let events0 = sched.evaluate_at(&prev_cfg, t0);
        assert_eq!(events0.len(), 2);

        // 同一分钟 18:30:15 保存了其他设置（闹钟时间未改），再评估不应重复触发
        let mut next_cfg = prev_cfg.clone();
        next_cfg.volume = 65;
        sched.sync_slots_after_config_save(&prev_cfg, &next_cfg);
        let t1 = Local.with_ymd_and_hms(2026, 11, 18, 18, 30, 15).unwrap();
        let events1 = sched.evaluate_at(&next_cfg, t1);
        assert!(
            events1.is_empty(),
            "same-minute save without changing time should not retrigger"
        );

        // 若将同一闹钟改到另一个时间后改回 18:30（或时间发生变更），仅闹钟槽位会被清理允许重新评估
        let mut edited_cfg = next_cfg.clone();
        edited_cfg.alarms[0].minute = 31;
        sched.sync_slots_after_config_save(&next_cfg, &edited_cfg);
        assert!(!sched.last_alarm_slots.contains_key(&1));
        assert!(sched.last_chime_slot.is_some());
    }

    #[test]
    fn test_leak_monitor_detects_five_consecutive_monotonic_cycles_and_logs_refs() {
        let mut sched = Scheduler::new();
        let cfg = AppConfig::default();

        sched.set_runtime_refs(ScheduledRefContext {
            app_state_strong: 2,
            draft_alarms_strong: 2,
            web_console_strong: 2,
            alarm_alert_slot_strong: 2,
            single_instance_guard_strong: 2,
            voice_preview_gen_strong: 2,
            tray_arc_strong: 3,
            ipc_wake_arc_strong: 2,
            perf_sample_arc_strong: 2,
            cpu_throttled_arc_strong: 2,
            update_github_token_strong: 2,
            web_console_running: true,
            alarm_alert_alive: false,
            visible_windows_mask: 0b00001,
            audio: AudioRefSnapshot {
                gen_counter_strong: 3,
                active_seq_gen_strong: 3,
                live_volume_strong: 3,
                tts_cache_strong: 3,
                buffer_pool_strong: 3,
                tts_status_tx_strong: 2,
                active_sink_present: false,
                active_seq_gen: 0,
                tts_cache_entries: 4,
                tts_cache_bytes: 64 * 1024,
                pool_byte_buffers: 2,
                pool_byte_capacity: 512 * 1024,
                pool_sample_queues: 1,
                pool_sample_capacity: 8192,
                pool_byte_hits: 10,
                pool_byte_misses: 2,
                pool_sample_hits: 15,
                pool_sample_misses: 1,
            },
        });

        let base_t = Local.with_ymd_and_hms(2026, 11, 18, 19, 0, 10).unwrap();
        let base_mem = 40 * 1024 * 1024u64;

        // Baseline (cycle 0) + cycles 1..=4: not yet 5 consecutive increases
        for i in 0..5u64 {
            let t = base_t + Duration::seconds((i * 60) as i64);
            let mem = base_mem + i * 128 * 1024;
            assert!(
                sched.check_leak_at_cycle(&cfg, t, mem, mem).is_none(),
                "should not alert before 5 consecutive increases (step {i})"
            );
        }

        // Cycle 5: 5th consecutive monotonic increase -> triggers detailed object-ref report
        let t5 = base_t + Duration::seconds(5 * 60);
        let mem5 = base_mem + 5 * 128 * 1024;
        let report = sched
            .check_leak_at_cycle(&cfg, t5, mem5, mem5)
            .expect("expected leak report at 5th consecutive monotonic increase");
        assert!(report.contains("5 consecutive scheduled cycles"));
        assert!(report.contains("scheduler="));
        assert!(report.contains("rc/arc="));
        assert!(report.contains("audio_engine_refs="));
        assert!(report.contains("pool="));
        assert!(report.contains("tts_cache="));
        assert!(report.contains("tray=3"));
        assert!(report.contains("gen_counter=3"));
    }

    #[test]
    fn test_leak_monitor_filters_transient_audio_lazy_window_and_resets_on_drop() {
        let mut sched = Scheduler::new();
        let cfg = AppConfig::default();
        let base_t = Local.with_ymd_and_hms(2026, 11, 18, 20, 0, 10).unwrap();
        let base_mem = 42 * 1024 * 1024u64;

        // Baseline + 3 rising cycles
        for i in 0..4u64 {
            let t = base_t + Duration::seconds((i * 60) as i64);
            let mem = base_mem + i * 128 * 1024;
            assert!(sched.check_leak_at_cycle(&cfg, t, mem, mem).is_none());
        }

        // Cycle 4: active audio playback should be ignored cleanly without false alert
        let mut busy_refs = ScheduledRefContext::default();
        busy_refs.audio.active_sink_present = true;
        busy_refs.audio.active_seq_gen = 7;
        sched.set_runtime_refs(busy_refs);
        assert!(sched
            .check_leak_at_cycle(
                &cfg,
                base_t + Duration::seconds(4 * 60),
                base_mem + 900 * 1024,
                base_mem + 900 * 1024
            )
            .is_none());

        // Cycle 5: lazy AlarmAlertWindow creation resets baseline instead of triggering leak alert
        sched.set_runtime_refs(ScheduledRefContext {
            alarm_alert_alive: true,
            ..ScheduledRefContext::default()
        });
        assert!(sched
            .check_leak_at_cycle(
                &cfg,
                base_t + Duration::seconds(5 * 60),
                base_mem + 1200 * 1024,
                base_mem + 1200 * 1024
            )
            .is_none());

        // Cycle 6: memory drop resets streak
        assert!(sched
            .check_leak_at_cycle(
                &cfg,
                base_t + Duration::seconds(6 * 60),
                base_mem + 500 * 1024,
                base_mem + 500 * 1024
            )
            .is_none());
        assert_eq!(sched.leak_monitor.samples.len(), 1);
    }
}
