use crate::config::{render_holiday_template, render_tts_template, AppConfig};
use crate::win_sys;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Timelike};
use serde::Deserialize;

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

    pub fn rule_key(self) -> String {
        if self.is_leap {
            format!("L{:02}-{:02}*", self.month, self.day)
        } else {
            format!("L{:02}-{:02}", self.month, self.day)
        }
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
    EsportsFest,
    Custom,
}

/// 匹配到的节假日及定制问候信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolidayInfo {
    pub name: String,
    pub kind: HolidayKind,
    pub greeting: String,
    pub custom_audio_path: Option<String>,
}

/// 外部节假日 JSON 数据接口条目（兼容本地/局域网/HTTPS 接口下发门店活动或调休假日）
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ExternalHolidayEntry {
    /// 日期匹配键：支持 "YYYY-MM-DD"、"MM-DD" 或农历 "LMM-DD"（如 "L01-01"）
    pub date: String,
    /// 节日或活动名称
    pub name: String,
    /// 专属问候语文本
    #[serde(default)]
    pub greeting: String,
    /// 可选：该节日专属音频文件路径
    #[serde(default)]
    pub audio_path: String,
    /// 若为 false（如调休补班标记），则不作为节日触发
    #[serde(default)]
    pub is_holiday: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct HolidayFeedEnvelope {
    #[serde(default)]
    holidays: Vec<ExternalHolidayEntry>,
}

/// 解析外部节假日 JSON（支持顶层数组 `[...]` 或 `{ "holidays": [...] }` 包装格式）
pub fn parse_holiday_feed_json(raw: &str) -> Result<Vec<ExternalHolidayEntry>, String> {
    let trimmed = raw.trim().trim_start_matches('\u{feff}');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(list) = serde_json::from_str::<Vec<ExternalHolidayEntry>>(trimmed) {
        return Ok(list);
    }
    if let Ok(env) = serde_json::from_str::<HolidayFeedEnvelope>(trimmed) {
        return Ok(env.holidays);
    }
    Err("节假日 JSON 格式无效：须为数组或包含 holidays 字段的对象".to_string())
}

/// 从外部 HTTP(S) 接口或本地/共享 JSON 文件加载节假日数据（内置 Windows 代理兼容与 5 秒超时）
pub fn fetch_holiday_feed_from_source(source: &str) -> Result<Vec<ExternalHolidayEntry>, String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let raw_json = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout_read(std::time::Duration::from_secs(6));
        if let Some(proxy_url) = win_sys::read_windows_proxy_url() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                builder = builder.proxy(proxy);
            }
        }
        let agent = builder.build();
        let resp = agent
            .get(trimmed)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| format!("拉取节假日接口失败：{e}"))?;
        resp.into_string()
            .map_err(|e| format!("读取节假日接口响应失败：{e}"))?
    } else {
        let path_str = trimmed.strip_prefix("file:///").unwrap_or(trimmed);
        std::fs::read_to_string(path_str)
            .map_err(|e| format!("读取本地节假日数据文件失败：{e}"))?
    };

    let entries = parse_holiday_feed_json(&raw_json)?;
    win_sys::log_info(
        "holiday",
        &format!("loaded {} holiday feed entries from \"{trimmed}\"", entries.len()),
    );
    Ok(entries)
}

/// 解析设置面板中的自定义节日规则字符串
/// 格式支持换行或分号分隔：`11-14=电竞周年庆|全场开黑福利开启|D:\audio\anniv.wav` 或 `L01-01=新春特惠`
pub fn parse_custom_holiday_rules(raw: &str) -> Vec<ExternalHolidayEntry> {
    let mut entries = Vec::new();
    for segment in raw.split(['\n', ';', '；']) {
        let line = segment.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((date_part, rest)) = line.split_once('=') else {
            continue;
        };
        let date_key = date_part.trim();
        if date_key.is_empty() {
            continue;
        }
        let mut cols = rest.split('|').map(str::trim);
        let name = cols.next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let greeting = cols.next().unwrap_or("").to_string();
        let audio_path = cols.next().unwrap_or("").to_string();
        entries.push(ExternalHolidayEntry {
            date: date_key.to_string(),
            name,
            greeting,
            audio_path,
            is_holiday: Some(true),
        });
    }
    entries
}

fn entry_matches_date(entry: &ExternalHolidayEntry, date: NaiveDate, lunar: Option<LunarDate>) -> bool {
    let key = entry.date.trim();
    if key.eq_ignore_ascii_case(&date.format("%Y-%m-%d").to_string())
        || key.eq_ignore_ascii_case(&date.format("%m-%d").to_string())
    {
        return true;
    }
    if let Some(l) = lunar {
        if !l.is_leap {
            let l_key = l.rule_key();
            if key.eq_ignore_ascii_case(&l_key) {
                return true;
            }
        }
    }
    false
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

/// 内置中国法定节假日、传统农历节日与电竞/网吧热门节庆库
pub fn lookup_builtin_holiday(date: NaiveDate, lunar: Option<LunarDate>) -> Option<HolidayInfo> {
    // 1. 优先判断除夕（明天为农历正月初一，兼容大年廿九与大年三十）
    if let Some(tomorrow) = date.checked_add_days(chrono::Days::new(1)) {
        if let Some(tl) = solar_to_lunar(tomorrow) {
            if !tl.is_leap && tl.month == 1 && tl.day == 1 {
                return Some(HolidayInfo {
                    name: "除夕".to_string(),
                    kind: HolidayKind::Statutory,
                    greeting: "除夕守岁迎新春，开黑团战祝您把把超神！".to_string(),
                    custom_audio_path: None,
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
                    "新春大吉，祝各位玩家新年上分顺利、万事如意！",
                )),
                (1, 15) => Some((
                    "元宵节",
                    HolidayKind::Traditional,
                    "元宵佳节喜团圆，祝您五杀连连、欧气满满！",
                )),
                (5, 5) => Some((
                    "端午节",
                    HolidayKind::Statutory,
                    "端午安康，一举高粽，排位对战轻松连胜！",
                )),
                (7, 7) => Some((
                    "七夕节",
                    HolidayKind::Traditional,
                    "七夕快乐，愿甜蜜双排甜蜜常伴、快乐游戏！",
                )),
                (8, 15) => Some((
                    "中秋节",
                    HolidayKind::Statutory,
                    "中秋花好月圆，祝大家欢聚一堂、心想事成！",
                )),
                (9, 9) => Some((
                    "重阳节",
                    HolidayKind::Traditional,
                    "九九重阳敬长辈，祝愿阖家安康、吉祥顺遂！",
                )),
                (12, 8) => Some((
                    "腊八节",
                    HolidayKind::Traditional,
                    "过了腊八就是年，天冷注意保暖休息！",
                )),
                _ => None,
            };
            if let Some((name, kind, greeting)) = hit {
                return Some(HolidayInfo {
                    name: name.to_string(),
                    kind,
                    greeting: greeting.to_string(),
                    custom_audio_path: None,
                });
            }
        }
    }

    // 3. 公历法定节假日与清明节
    if is_qingming(date) {
        return Some(HolidayInfo {
            name: "清明节".to_string(),
            kind: HolidayKind::Statutory,
            greeting: "清明春和景明，踏青休闲注意安全！".to_string(),
            custom_audio_path: None,
        });
    }

    let solar_hit = match (date.month(), date.day()) {
        (1, 1) => Some((
            "元旦",
            HolidayKind::Statutory,
            "新年新气象，元旦快乐，祝您新的一年旗开得胜！",
        )),
        (5, 1) => Some((
            "劳动节",
            HolidayKind::Statutory,
            "五一劳动节快乐，劳逸结合畅享快乐游戏时光！",
        )),
        (10, 1) => Some((
            "国庆节",
            HolidayKind::Statutory,
            "欢度国庆佳节，假期开黑愉快、祝祖国繁荣昌盛！",
        )),
        (5, 20) => Some((
            "520 电竞开黑节",
            HolidayKind::EsportsFest,
            "520 浪漫开黑日，祝各位大神默契配合、连胜不停！",
        )),
        (11, 11) => Some((
            "双十一狂欢日",
            HolidayKind::EsportsFest,
            "双十一狂欢节，祝大家手气爆棚、局局顺风！",
        )),
        (12, 24) => Some((
            "平安夜",
            HolidayKind::EsportsFest,
            "平安夜温馨送福，祝各位玩家畅玩顺遂、平平安安！",
        )),
        (12, 25) => Some((
            "圣诞节",
            HolidayKind::EsportsFest,
            "圣诞快乐，节庆氛围拉满，祝您把把 MVP！",
        )),
        _ => None,
    };

    solar_hit.map(|(name, kind, greeting)| HolidayInfo {
        name: name.to_string(),
        kind,
        greeting: greeting.to_string(),
        custom_audio_path: None,
    })
}

/// 综合解析指定日期的节日（优先级：本地自定义规则 > 外部节假日 JSON 数据接口 > 内置农历/公历节假日）
pub fn resolve_holiday_for_date(
    date: NaiveDate,
    custom_rules: &str,
    external_entries: &[ExternalHolidayEntry],
) -> Option<HolidayInfo> {
    let lunar = solar_to_lunar(date);
    let custom_entries = parse_custom_holiday_rules(custom_rules);

    for entry in custom_entries.iter().chain(external_entries.iter()) {
        if entry_matches_date(entry, date, lunar) {
            if entry.is_holiday == Some(false) {
                return None;
            }
            let greeting = if entry.greeting.trim().is_empty() {
                format!("祝您{}快乐，游戏顺遂！", entry.name)
            } else {
                entry.greeting.clone()
            };
            let audio_path = {
                let p = entry.audio_path.trim();
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                }
            };
            return Some(HolidayInfo {
                name: entry.name.clone(),
                kind: HolidayKind::Custom,
                greeting,
                custom_audio_path: audio_path,
            });
        }
    }

    lookup_builtin_holiday(date, lunar)
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
    Meal {
        repeat: u8,
        /// 开启“餐饮提醒文字转语音”开关时生成的播报文本
        spoken_text: Option<String>,
    },
}

/// 主界面倒计时摘要信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextEventInfo {
    pub title: String,
    pub countdown_text: String,
    pub target_time_text: String,
}

/// 核心定时调度器（负责分钟节拍识别、防重入、跨日静默判定、农历节假日接口与倒计时计算）
#[derive(Debug, Default)]
pub struct Scheduler {
    last_chime_slot: Option<(i32, u32, u32, u32)>,
    last_meal_slot: Option<(i32, u32, u32, u32)>,
    external_holidays: Vec<ExternalHolidayEntry>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入从本地/远程节假日数据接口拉取的节假日或门店活动列表
    pub fn set_external_holidays(&mut self, entries: Vec<ExternalHolidayEntry>) {
        self.external_holidays = entries;
    }

    pub fn external_holiday_count(&self) -> usize {
        self.external_holidays.len()
    }

    /// 校时后重置防重入槽位，防止系统时间回调后错过提醒
    pub fn reset_slots(&mut self) {
        self.last_chime_slot = None;
        self.last_meal_slot = None;
    }

    /// 查询指定公历日期命中的节假日（按配置自定义规则 > 外部数据接口 > 内置农历节假日）
    pub fn lookup_holiday(&self, date: NaiveDate, cfg: &AppConfig) -> Option<HolidayInfo> {
        resolve_holiday_for_date(date, &cfg.holiday_custom_rules, &self.external_holidays)
    }

    /// 生成当天历法摘要（供主窗日期栏及设置页“今日历法预览”使用）
    pub fn describe_day_calendar(&self, date: NaiveDate, cfg: &AppConfig) -> String {
        let lunar_text = solar_to_lunar(date)
            .map(|l| l.format_zh())
            .unwrap_or_else(|| "农历数据外年份".to_string());
        if let Some(h) = self.lookup_holiday(date, cfg) {
            format!("{lunar_text} · 节日：{name}", name = h.name)
        } else {
            format!("{lunar_text} · 常规工作日/周末")
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

    /// 根据用户开关与当前系统时间生成餐饮提醒播报文本
    pub fn build_meal_announcement(
        tts_enabled: bool,
        text: &str,
        now: DateTime<Local>,
    ) -> Option<String> {
        if !tts_enabled {
            return None;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(render_tts_template(trimmed, now.hour(), now.minute()))
    }

    /// 判断给定 (hour, minute) 是否位于配置的静默时段内（FR-05 / FR-06）
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

    /// 周期检测（建议每 250ms–500ms 调用一次），返回本拍需要触发的事件列表
    pub fn evaluate_at(
        &mut self,
        cfg: &AppConfig,
        now: DateTime<Local>,
    ) -> Vec<TriggerEvent> {
        let mut events = Vec::new();
        let slot = (now.year(), now.ordinal(), now.hour(), now.minute());
        let minute_u8 = now.minute() as u8;
        let hour_u8 = now.hour() as u8;

        // 1. 报时检测（受静音模式与静默时段约束；开启节假日模式时自动匹配专属报时音与问候语）
        if cfg.chime_minutes.contains(&minute_u8) && self.last_chime_slot != Some(slot) {
            self.last_chime_slot = Some(slot);
            if !cfg.mute_mode && !Self::is_quiet_at(cfg, now.hour(), now.minute()) {
                let holiday = if cfg.holiday_enabled {
                    self.lookup_holiday(now.date_naive(), cfg)
                } else {
                    None
                };

                let spoken_time_text =
                    Self::build_chime_announcement_with_holiday(cfg, now, holiday.as_ref());

                let holiday_audio_path = if cfg.holiday_enabled {
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

                events.push(TriggerEvent::Chime {
                    repeat: cfg.chime_repeat_count,
                    spoken_time_text,
                    holiday_audio_path,
                    holiday_name: holiday.map(|h| h.name),
                });
            }
        }

        // 2. 餐饮提醒检测（受独立开关与托盘静音模式约束）
        if cfg.meal_enabled
            && hour_u8 == cfg.meal_hour
            && minute_u8 == cfg.meal_minute
            && self.last_meal_slot != Some(slot)
        {
            self.last_meal_slot = Some(slot);
            if !cfg.mute_mode {
                let spoken_text = Self::build_meal_announcement(
                    cfg.meal_tts_enabled,
                    &cfg.meal_tts_text,
                    now,
                );
                events.push(TriggerEvent::Meal {
                    repeat: cfg.meal_repeat_count,
                    spoken_text,
                });
            }
        }

        events
    }

    /// 计算距离下一个“整/半点报时”或“餐饮提醒”的精确倒计时（FR-19）
    pub fn compute_next_event(cfg: &AppConfig, now: DateTime<Local>) -> NextEventInfo {
        let mut candidates: Vec<(DateTime<Local>, &'static str)> = Vec::new();

        // 向前搜索最多 48 小时内的下一个有效报时时刻（自动跳过静默时段）
        if !cfg.chime_minutes.is_empty() {
            let base = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now)
                + Duration::minutes(1);

            for offset_min in 0..(48 * 60) {
                let candidate = base + Duration::minutes(offset_min);
                let m = candidate.minute() as u8;
                if cfg.chime_minutes.contains(&m)
                    && !Self::is_quiet_at(cfg, candidate.hour(), candidate.minute())
                {
                    let label = if m == 0 {
                        "整点报时"
                    } else if m == 30 {
                        "半点报时"
                    } else {
                        "定时报时"
                    };
                    candidates.push((candidate, label));
                    break;
                }
            }
        }

        // 计算下一个餐饮提醒时刻
        if cfg.meal_enabled {
            let today_meal = now
                .date_naive()
                .and_hms_opt(cfg.meal_hour as u32, cfg.meal_minute as u32, 0)
                .and_then(|naive| naive.and_local_timezone(Local).single());

            if let Some(t) = today_meal {
                let target = if t > now { t } else { t + Duration::days(1) };
                candidates.push((target, "餐饮提醒"));
            }
        }

        candidates.sort_by_key(|(dt, _)| *dt);

        if let Some((target, label)) = candidates.into_iter().next() {
            let secs = (target - now).num_seconds().max(0);
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            let rem_secs = secs % 60;
            let countdown_text = if hours > 0 {
                format!("{hours:02}:{mins:02}:{rem_secs:02}")
            } else {
                format!("{mins:02}分{rem_secs:02}秒")
            };
            NextEventInfo {
                title: label.to_string(),
                countdown_text,
                target_time_text: target.format("%H:%M").to_string(),
            }
        } else {
            NextEventInfo {
                title: "暂无待执行提醒".to_string(),
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
    fn test_same_day_quiet_period() {
        let mut cfg = AppConfig::default();
        cfg.quiet_enabled = true;
        cfg.quiet_start_hour = 0;
        cfg.quiet_start_minute = 0;
        cfg.quiet_end_hour = 8;
        cfg.quiet_end_minute = 0;

        assert!(Scheduler::is_quiet_at(&cfg, 0, 0));
        assert!(Scheduler::is_quiet_at(&cfg, 7, 59));
        assert!(!Scheduler::is_quiet_at(&cfg, 8, 0));
        assert!(!Scheduler::is_quiet_at(&cfg, 23, 30));
    }

    #[test]
    fn test_evaluate_deduplication_and_quiet_skip() {
        let mut cfg = AppConfig::default();
        cfg.quiet_enabled = true;
        cfg.quiet_start_hour = 0;
        cfg.quiet_start_minute = 0;
        cfg.quiet_end_hour = 8;
        cfg.quiet_end_minute = 0;
        cfg.meal_enabled = true;
        cfg.meal_hour = 12;
        cfg.meal_minute = 0;
        cfg.meal_repeat_count = 2;

        let mut sched = Scheduler::new();

        let t_quiet = Local.with_ymd_and_hms(2026, 11, 13, 7, 30, 0).unwrap();
        assert!(sched.evaluate_at(&cfg, t_quiet).is_empty());

        let t_noon = Local.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let events = sched.evaluate_at(&cfg, t_noon);
        assert_eq!(
            events,
            vec![
                TriggerEvent::Chime {
                    repeat: 1,
                    spoken_time_text: None,
                    holiday_audio_path: None,
                    holiday_name: None,
                },
                TriggerEvent::Meal {
                    repeat: 2,
                    spoken_text: None,
                },
            ]
        );

        let t_noon_next_sec = Local.with_ymd_and_hms(2026, 11, 13, 12, 0, 1).unwrap();
        assert!(sched.evaluate_at(&cfg, t_noon_next_sec).is_empty());
    }

    #[test]
    fn test_mute_mode_silences_events() {
        let mut cfg = AppConfig::default();
        cfg.mute_mode = true;
        cfg.meal_enabled = true;
        cfg.meal_hour = 12;
        cfg.meal_minute = 0;

        let mut sched = Scheduler::new();
        let noon = Local.with_ymd_and_hms(2026, 11, 14, 12, 0, 5).unwrap();
        assert!(sched.evaluate_at(&cfg, noon).is_empty());

        cfg.mute_mode = false;
        assert!(sched.evaluate_at(&cfg, noon).is_empty());

        let half = Local.with_ymd_and_hms(2026, 11, 14, 12, 30, 0).unwrap();
        assert_eq!(
            sched.evaluate_at(&cfg, half),
            vec![TriggerEvent::Chime {
                repeat: 1,
                spoken_time_text: None,
                holiday_audio_path: None,
                holiday_name: None,
            }]
        );
    }

    #[test]
    fn test_chime_tts_toggle_and_real_time_formatting() {
        let mut cfg = AppConfig::default();
        cfg.chime_tts_enabled = true;
        cfg.chime_tts_template = "现在时间，{TIME}".to_string();

        let mut sched = Scheduler::new();
        let t_half = Local.with_ymd_and_hms(2026, 11, 14, 14, 30, 0).unwrap();
        assert_eq!(
            sched.evaluate_at(&cfg, t_half),
            vec![TriggerEvent::Chime {
                repeat: 1,
                spoken_time_text: Some("现在时间，十四点三十分".to_string()),
                holiday_audio_path: None,
                holiday_name: None,
            }]
        );

        cfg.chime_tts_enabled = false;
        let t_hour = Local.with_ymd_and_hms(2026, 11, 14, 15, 0, 0).unwrap();
        assert_eq!(
            sched.evaluate_at(&cfg, t_hour),
            vec![TriggerEvent::Chime {
                repeat: 1,
                spoken_time_text: None,
                holiday_audio_path: None,
                holiday_name: None,
            }]
        );
    }

    #[test]
    fn test_lunar_conversion_and_builtin_holidays() {
        // 2025 闰六月初一 -> 2025-07-25
        let leap_6_1 = solar_to_lunar(NaiveDate::from_ymd_opt(2025, 7, 25).unwrap()).unwrap();
        assert_eq!(
            leap_6_1,
            LunarDate {
                year: 2025,
                month: 6,
                day: 1,
                is_leap: true,
            }
        );
        assert_eq!(leap_6_1.format_zh(), "农历闰六月初一");

        // 2025 中秋节（2025-10-06 为农历八月十五）
        let mid_autumn_2025 = NaiveDate::from_ymd_opt(2025, 10, 6).unwrap();
        let l_ma = solar_to_lunar(mid_autumn_2025).unwrap();
        assert_eq!((l_ma.month, l_ma.day, l_ma.is_leap), (8, 15, false));
        let h_ma = lookup_builtin_holiday(mid_autumn_2025, Some(l_ma)).unwrap();
        assert_eq!(h_ma.name, "中秋节");

        // 2025 除夕（2026-02-16 为腊月廿九，次日 2026-02-17 为春节正月初一）
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
    fn test_custom_holiday_feed_and_chime_trigger() {
        let json = r#"{
            "holidays": [
                {
                    "date": "2026-11-14",
                    "name": "网吧电竞周年庆",
                    "greeting": "全场饮品八折特惠，祝各位召唤师连胜上分！",
                    "audio_path": "D:\\audio\\esports_anniv.wav"
                }
            ]
        }"#;
        let feed = parse_holiday_feed_json(json).unwrap();
        assert_eq!(feed.len(), 1);

        let mut sched = Scheduler::new();
        sched.set_external_holidays(feed);

        let mut cfg = AppConfig::default();
        cfg.holiday_enabled = true;
        cfg.holiday_custom_audio_path = "D:\\audio\\fallback_fest.wav".to_string();
        cfg.holiday_greeting_template = "今天是{HOLIDAY}（{LUNAR}），{GREETING}现在时间，{TIME}".to_string();

        let now = Local.with_ymd_and_hms(2026, 11, 14, 18, 30, 0).unwrap();
        let events = sched.evaluate_at(&cfg, now);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TriggerEvent::Chime {
                repeat,
                spoken_time_text,
                holiday_audio_path,
                holiday_name,
            } => {
                assert_eq!(*repeat, 1);
                assert_eq!(holiday_name.as_deref(), Some("网吧电竞周年庆"));
                assert_eq!(
                    holiday_audio_path.as_deref(),
                    Some("D:\\audio\\esports_anniv.wav")
                );
                let text = spoken_time_text.as_deref().unwrap();
                assert!(text.contains("网吧电竞周年庆"));
                assert!(text.contains("农历十月初六"));
                assert!(text.contains("十八点三十分"));
            }
            _ => panic!("expected TriggerEvent::Chime"),
        }

        // 内联自定义规则优先覆盖外部数据接口
        cfg.holiday_custom_rules = "11-14=冠军争霸夜|五人开黑免单抽奖中".to_string();
        let now2 = Local.with_ymd_and_hms(2026, 11, 14, 19, 0, 0).unwrap();
        let events2 = sched.evaluate_at(&cfg, now2);
        match &events2[0] {
            TriggerEvent::Chime {
                holiday_name,
                holiday_audio_path,
                ..
            } => {
                assert_eq!(holiday_name.as_deref(), Some("冠军争霸夜"));
                assert_eq!(
                    holiday_audio_path.as_deref(),
                    Some("D:\\audio\\fallback_fest.wav")
                );
            }
            _ => panic!("expected TriggerEvent::Chime"),
        }
    }
}
