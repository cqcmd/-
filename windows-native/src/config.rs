use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 支持的三套内置视觉主题
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// 暗夜曜石（低亮度深色风格，适合夜间或暗光环境）
    ObsidianDark,
    /// 晨曦云母（Win11 极简浅色风格）
    Win11Light,
    /// 暖金咖啡馆（默认：低蓝光护眼风格）
    #[default]
    WarmLounge,
}

impl ThemeMode {
    pub fn to_index(self) -> i32 {
        match self {
            ThemeMode::ObsidianDark => 0,
            ThemeMode::Win11Light => 1,
            ThemeMode::WarmLounge => 2,
        }
    }

    pub fn from_index(idx: i32) -> Self {
        match idx {
            1 => ThemeMode::Win11Light,
            2 => ThemeMode::WarmLounge,
            _ => ThemeMode::ObsidianDark,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ThemeMode::ObsidianDark => "暗夜曜石",
            ThemeMode::Win11Light => "晨曦云母",
            ThemeMode::WarmLounge => "暖金咖啡馆",
        }
    }
}

/// 语音合成引擎（默认本地离线 SAPI5，可选 Microsoft Edge 在线语音）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngine {
    /// Windows 原生 SAPI5 离线引擎（默认首选：零外网请求、无需联网、纯本地毫秒级播报）
    #[default]
    Sapi5,
    /// Microsoft Edge TTS 在线神经网络语音（备选，网络异常或受限时自动回退至本地 SAPI5）
    EdgeTts,
}

impl TtsEngine {
    pub fn to_index(self) -> i32 {
        match self {
            TtsEngine::Sapi5 => 0,
            TtsEngine::EdgeTts => 1,
        }
    }

    pub fn from_index(idx: i32) -> Self {
        match idx {
            1 => TtsEngine::EdgeTts,
            _ => TtsEngine::Sapi5,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            TtsEngine::Sapi5 => "本地 Windows 语音（纯离线）",
            TtsEngine::EdgeTts => "微软 Edge 在线语音（备选）",
        }
    }
}

pub const DEFAULT_EDGE_TTS_VOICE: &str = "zh-CN-XiaoxiaoNeural";

pub fn edge_voice_from_index(idx: i32) -> &'static str {
    match idx {
        1 => "zh-CN-YunxiNeural",
        2 => "zh-CN-YunyangNeural",
        3 => "zh-CN-XiaoyiNeural",
        _ => DEFAULT_EDGE_TTS_VOICE,
    }
}

pub fn edge_voice_to_index(voice: &str) -> i32 {
    match voice.trim() {
        "zh-CN-YunxiNeural" => 1,
        "zh-CN-YunyangNeural" => 2,
        "zh-CN-XiaoyiNeural" => 3,
        _ => 0,
    }
}

/// 自定义闹钟重复周期模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlarmRepeatMode {
    /// 仅一次（到达目标时间触发后自动关闭，避免次日重复响铃）
    Once,
    /// 每天
    #[default]
    Daily,
    /// 工作日（周一至周五）
    Workdays,
    /// 周末（周六、周日）
    Weekends,
    /// 自定义星期（周一至周日自由勾选）
    Weekly,
    /// 每隔 N 天（从起始锚点日期开始按固定天间隔循环）
    IntervalDays,
}

impl AlarmRepeatMode {
    pub fn to_index(self) -> i32 {
        match self {
            AlarmRepeatMode::Once => 0,
            AlarmRepeatMode::Daily => 1,
            AlarmRepeatMode::Workdays => 2,
            AlarmRepeatMode::Weekends => 3,
            AlarmRepeatMode::Weekly => 4,
            AlarmRepeatMode::IntervalDays => 5,
        }
    }

    pub fn from_index(idx: i32) -> Self {
        match idx {
            0 => AlarmRepeatMode::Once,
            1 => AlarmRepeatMode::Daily,
            2 => AlarmRepeatMode::Workdays,
            3 => AlarmRepeatMode::Weekends,
            4 => AlarmRepeatMode::Weekly,
            5 => AlarmRepeatMode::IntervalDays,
            _ => AlarmRepeatMode::Daily,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AlarmRepeatMode::Once => "仅一次",
            AlarmRepeatMode::Daily => "每天",
            AlarmRepeatMode::Workdays => "工作日",
            AlarmRepeatMode::Weekends => "周末",
            AlarmRepeatMode::Weekly => "自定义星期",
            AlarmRepeatMode::IntervalDays => "每隔 N 天",
        }
    }
}

/// 自定义定时闹钟条目（支持多种时间周期、提示音、自定义音频与文字转语音）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CustomAlarm {
    pub id: u64,
    pub label: String,
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    pub repeat_mode: AlarmRepeatMode,
    /// 自定义星期勾选：[周一, 周二, 周三, 周四, 周五, 周六, 周日]
    pub weekdays: [bool; 7],
    /// 每隔 N 天（1..=365，仅 repeat_mode == IntervalDays 时有效）
    pub interval_days: u16,
    /// 间隔起始日期 "YYYY-MM-DD"（仅 repeat_mode == IntervalDays 时作为起点）
    pub anchor_date: String,
    /// 单次闹钟目标日期 "YYYY-MM-DD"（仅 repeat_mode == Once 时有效）
    pub once_date: String,
    /// 播放重复次数（1..=10）
    pub repeat_count: u8,
    /// 播报音频或语音前是否先播放提示音
    pub play_chime_before: bool,
    /// 是否启用文字转语音播报
    pub tts_enabled: bool,
    /// 文字转语音内容（支持 {TIME} / {TIME12} / {PERIOD} / {LABEL} / {HH}:{MM}）
    pub tts_text: String,
    /// 可选本机自定义音频路径（WAV / MP3 / OGG / FLAC）
    pub custom_audio_path: String,
    /// 是否受静默免打扰时段约束（默认 false，防止早晨/重要闹钟被夜间静默跳过）
    pub respect_quiet_hours: bool,
    /// 响铃时是否弹出桌面提醒窗口（默认 false，仅播放声音/语音）
    pub show_alert: bool,
}

impl Default for CustomAlarm {
    fn default() -> Self {
        Self {
            id: 1,
            label: "日常提醒".to_string(),
            enabled: true,
            hour: 9,
            minute: 0,
            repeat_mode: AlarmRepeatMode::Daily,
            weekdays: [true, true, true, true, true, false, false],
            interval_days: 2,
            anchor_date: String::new(),
            once_date: String::new(),
            repeat_count: 1,
            play_chime_before: true,
            tts_enabled: true,
            tts_text: "现在时间，{TIME}，{LABEL}".to_string(),
            custom_audio_path: String::new(),
            respect_quiet_hours: false,
            show_alert: false,
        }
    }
}

impl CustomAlarm {
    pub fn format_time(&self) -> String {
        format!("{:02}:{:02}", self.hour.min(23), self.minute.min(59))
    }

    pub fn format_cycle_summary(&self) -> String {
        match self.repeat_mode {
            AlarmRepeatMode::Once => {
                if self.once_date.trim().is_empty() {
                    "仅一次".to_string()
                } else {
                    let short = self
                        .once_date
                        .split('-')
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join("-");
                    if short.is_empty() {
                        "仅一次".to_string()
                    } else {
                        format!("仅一次 ({short})")
                    }
                }
            }
            AlarmRepeatMode::Daily => "每天".to_string(),
            AlarmRepeatMode::Workdays => "工作日 (周一至五)".to_string(),
            AlarmRepeatMode::Weekends => "周末 (周六/日)".to_string(),
            AlarmRepeatMode::Weekly => {
                const NAMES: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
                let picked: Vec<&str> = self
                    .weekdays
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &on)| if on { Some(NAMES[i]) } else { None })
                    .collect();
                if picked.is_empty() {
                    "每周 (未选)".to_string()
                } else {
                    format!("每周 {}", picked.join(" "))
                }
            }
            AlarmRepeatMode::IntervalDays => {
                let n = self.interval_days.max(1);
                if n == 1 {
                    "每 1 天".to_string()
                } else {
                    format!("每 {n} 天")
                }
            }
        }
    }

    pub fn format_audio_badge(&self) -> String {
        let mut parts = Vec::new();
        if self.play_chime_before {
            parts.push("提示音");
        }
        if !self.custom_audio_path.trim().is_empty() {
            parts.push("自定义音频");
        }
        if self.tts_enabled {
            parts.push("文字转语音");
        }
        if self.show_alert {
            parts.push("弹窗");
        }
        if parts.is_empty() {
            "仅提示".to_string()
        } else {
            parts.join(" + ")
        }
    }

    /// 规范化单次闹钟目标日期与间隔闹钟起始锚点（在用户保存或创建时调用）
    pub fn ensure_anchor_and_once_dates(&mut self, today: NaiveDate, now_hour: u32, now_minute: u32) {
        if self.repeat_mode == AlarmRepeatMode::IntervalDays {
            let valid_anchor = NaiveDate::parse_from_str(self.anchor_date.trim(), "%Y-%m-%d").is_ok();
            if !valid_anchor {
                self.anchor_date = today.format("%Y-%m-%d").to_string();
            }
        }
        if self.repeat_mode == AlarmRepeatMode::Once {
            let target_mins = u32::from(self.hour) * 60 + u32::from(self.minute);
            let cur_mins = now_hour * 60 + now_minute;
            let parsed = NaiveDate::parse_from_str(self.once_date.trim(), "%Y-%m-%d").ok();
            let need_recompute = match parsed {
                None => true,
                Some(d) => d < today || (d == today && target_mins < cur_mins),
            };
            if need_recompute {
                // 若目标分钟恰好等于当前分钟（例如刚踩整点保存测试），仍保留在今天触发而非推迟到明天
                let target_date = if target_mins >= cur_mins {
                    today
                } else {
                    today + Duration::days(1)
                };
                self.once_date = target_date.format("%Y-%m-%d").to_string();
            }
        }
    }

    /// 判断该闹钟在指定公历日期是否应当触发
    pub fn should_ring_on_date(&self, date: NaiveDate) -> bool {
        match self.repeat_mode {
            AlarmRepeatMode::Once => {
                if let Ok(target) = NaiveDate::parse_from_str(self.once_date.trim(), "%Y-%m-%d") {
                    date == target
                } else {
                    true
                }
            }
            AlarmRepeatMode::Daily => true,
            AlarmRepeatMode::Workdays => date.weekday().num_days_from_monday() < 5,
            AlarmRepeatMode::Weekends => date.weekday().num_days_from_monday() >= 5,
            AlarmRepeatMode::Weekly => {
                let idx = date.weekday().num_days_from_monday() as usize;
                self.weekdays.get(idx).copied().unwrap_or(false)
            }
            AlarmRepeatMode::IntervalDays => {
                let interval = i64::from(self.interval_days.max(1));
                if let Ok(anchor) = NaiveDate::parse_from_str(self.anchor_date.trim(), "%Y-%m-%d") {
                    let diff = (date - anchor).num_days();
                    diff >= 0 && (diff % interval) == 0
                } else {
                    true
                }
            }
        }
    }

    pub fn validate(&mut self) -> Result<(), String> {
        self.label = self.label.trim().to_string();
        if self.label.is_empty() {
            self.label = format!("闹钟 {}", self.format_time());
        }
        if self.hour > 23 || self.minute > 59 {
            return Err(format!(
                "闹钟「{}」的时间无效（小时 0–23，分钟 0–59）。",
                self.label
            ));
        }
        if !(1..=10).contains(&self.repeat_count) {
            return Err(format!(
                "闹钟「{}」的重复播放次数必须在 1–10 次之间。",
                self.label
            ));
        }
        if self.repeat_mode == AlarmRepeatMode::Weekly && !self.weekdays.iter().any(|&on| on) {
            return Err(format!(
                "闹钟「{}」选择了自定义星期，请至少勾选周一至周日中的一天。",
                self.label
            ));
        }
        if self.repeat_mode == AlarmRepeatMode::IntervalDays
            && !(1..=365).contains(&self.interval_days)
        {
            return Err(format!(
                "闹钟「{}」的间隔天数必须在 1–365 天之间。",
                self.label
            ));
        }

        self.tts_text = self.tts_text.trim().to_string();
        self.custom_audio_path = self.custom_audio_path.trim().to_string();
        self.anchor_date = self.anchor_date.trim().to_string();
        self.once_date = self.once_date.trim().to_string();

        if self.enabled {
            if self.tts_enabled && self.tts_text.is_empty() {
                return Err(format!(
                    "闹钟「{}」已开启文字转语音播报，请填写播报文字内容。",
                    self.label
                ));
            }
            if !self.play_chime_before
                && !self.tts_enabled
                && self.custom_audio_path.is_empty()
            {
                return Err(format!(
                    "闹钟「{}」请至少启用“提示音”、“文字转语音”或“自定义音频”中的一项。",
                    self.label
                ));
            }
        }

        Ok(())
    }
}

/// 本地持久化配置结构（通用型桌面报时与自定义闹钟工具）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    /// 功能勾选框：启用整点报时（每小时 :00），默认 true
    pub hourly_chime_enabled: bool,
    /// 功能勾选框：启用半点报时（每小时 :30），默认 true
    pub half_hour_chime_enabled: bool,
    /// 可选额外自定义报时分钟（除 0 与 30 之外的 0..=59 分钟），默认空
    pub custom_chime_minutes: Vec<u8>,
    /// 单次报时重复播放次数（1..=10），默认 1
    pub chime_repeat_count: u8,
    /// 整/半点报时是否播放内置提示音，默认 true
    pub chime_play_sound: bool,
    /// 整/半点报时是否启用语音播报真实时间/自定义文字，默认 false
    pub chime_tts_enabled: bool,
    /// 整/半点语音播报模板（支持 {TIME}、{TIME12}、{PERIOD}、{HH}、{MM}）
    pub chime_tts_template: String,

    /// 功能勾选框：启用内置公历/农历节假日自动播报（命中节日时切换问候语与提示音），默认 false
    pub holiday_enabled: bool,
    /// 节假日专属报时音文件路径（支持 WAV/MP3/OGG/FLAC，为空时沿用内置提示音）
    pub holiday_custom_audio_path: String,
    /// 节假日问候语播报模板（支持 {HOLIDAY}、{GREETING}、{LUNAR}、{TIME} 等变量）
    pub holiday_greeting_template: String,

    /// 托盘快捷全局静音模式（开启后暂不触发所有定时报时与闹钟播报），默认 false
    pub mute_mode: bool,
    /// 是否启用静默免打扰时段，默认 false
    pub quiet_enabled: bool,
    /// 静默开始时间（时、分），预设 00:00
    pub quiet_start_hour: u8,
    pub quiet_start_minute: u8,
    /// 静默结束时间（时、分），预设 08:00
    pub quiet_end_hour: u8,
    pub quiet_end_minute: u8,

    /// 自定义闹钟列表（支持多组闹钟、时间周期、提示音与文字转语音）
    pub alarms: Vec<CustomAlarm>,

    /// 语音合成引擎（SAPI5 离线 / Microsoft Edge TTS 在线）
    pub tts_engine: TtsEngine,
    /// Microsoft Edge TTS 默认音色名称
    pub edge_tts_voice: String,
    /// 语音播报语速（-5..=5，0 为标准语速）
    pub tts_rate: i8,

    /// 全局播报音量（0..=100），预设 80
    pub volume: u8,
    /// NTP 校时服务器地址，默认 ntp.aliyun.com
    pub ntp_server: String,
    /// 是否开机自启，默认 false
    pub auto_start: bool,
    /// 当前界面主题（默认暖金咖啡馆）
    pub theme: ThemeMode,

    /// 是否开启后台定期版本检查，默认 true
    pub update_check_enabled: bool,
    /// 更新清单接口地址（默认 Gitee 国内直连源，支持 Releases 与标准 HTTP/HTTPS version.json）
    pub update_check_url: String,
    /// 更新检查间隔（小时，1..=168），默认 6 小时
    pub update_check_interval_hours: u16,
    /// 可选访问令牌（私有仓库或内网鉴权选填，公开仓库留空即可）
    pub update_github_token: String,

    /// 是否开启本地 Web 控制台（127.0.0.1:3000），默认 false（按需手动开启）
    pub web_console_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            hourly_chime_enabled: true,
            half_hour_chime_enabled: true,
            custom_chime_minutes: Vec::new(),
            chime_repeat_count: 1,
            chime_play_sound: true,
            chime_tts_enabled: false,
            chime_tts_template: "现在时间，{TIME}".to_string(),
            holiday_enabled: false,
            holiday_custom_audio_path: String::new(),
            holiday_greeting_template: "今天是{HOLIDAY}，{GREETING}现在时间，{TIME}".to_string(),
            mute_mode: false,
            quiet_enabled: false,
            quiet_start_hour: 0,
            quiet_start_minute: 0,
            quiet_end_hour: 8,
            quiet_end_minute: 0,
            alarms: Vec::new(),
            tts_engine: TtsEngine::Sapi5,
            edge_tts_voice: DEFAULT_EDGE_TTS_VOICE.to_string(),
            tts_rate: 0,
            volume: 80,
            ntp_server: "ntp.aliyun.com".to_string(),
            auto_start: false,
            theme: ThemeMode::WarmLounge,
            update_check_enabled: true,
            update_check_url: crate::version::DEFAULT_UPDATE_URL.to_string(),
            update_check_interval_hours: 6,
            update_github_token: String::new(),
            web_console_enabled: false,
        }
    }
}

impl AppConfig {
    /// 解析逗号/中文逗号分隔的额外报时分钟（允许为空；0 与 30 请通过勾选框控制，此处自动去重兼容）
    pub fn parse_optional_extra_minutes(input: &str) -> Result<Vec<u8>, String> {
        let normalized = input.replace('，', ",").replace('、', ",");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let mut result = Vec::new();
        for token in trimmed.split(',') {
            let part = token.trim();
            if part.is_empty() {
                return Err("额外报时分钟格式有误：检测到多余的逗号分隔符。".to_string());
            }
            let val: u16 = part
                .parse()
                .map_err(|_| format!("“{part}”不是有效的整数，请输入 0–59 之间的数字。"))?;
            if val > 59 {
                return Err(format!("分钟数值 {val} 超出范围，有效范围为 0–59。"));
            }
            let minute = val as u8;
            if minute != 0 && minute != 30 && !result.contains(&minute) {
                result.push(minute);
            }
        }
        result.sort_unstable();
        Ok(result)
    }

    /// 兼容解析：如在测试或脚本中一次性传入 "0, 15, 30"，同步映射整点/半点与额外分钟
    #[allow(dead_code)]
    pub fn parse_chime_minutes(input: &str) -> Result<Vec<u8>, String> {
        let normalized = input.replace('，', ",").replace('、', ",");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for token in trimmed.split(',') {
            let part = token.trim();
            if part.is_empty() {
                return Err("报时分钟格式有误：检测到多余的逗号分隔符。".to_string());
            }
            let val: u16 = part
                .parse()
                .map_err(|_| format!("“{part}”不是有效的整数，请输入 0–59 之间的数字。"))?;
            if val > 59 {
                return Err(format!("分钟数值 {val} 超出范围，有效范围为 0–59。"));
            }
            let minute = val as u8;
            if !result.contains(&minute) {
                result.push(minute);
            }
        }
        result.sort_unstable();
        Ok(result)
    }

    /// 计算当前启用的全部报时分钟集合（由整点、半点勾选框及可选额外分钟共同组成）
    pub fn effective_chime_minutes(&self) -> Vec<u8> {
        let mut mins = Vec::new();
        if self.hourly_chime_enabled {
            mins.push(0);
        }
        if self.half_hour_chime_enabled {
            mins.push(30);
        }
        for &m in &self.custom_chime_minutes {
            if m <= 59 && !mins.contains(&m) {
                mins.push(m);
            }
        }
        mins.sort_unstable();
        mins
    }

    pub fn format_custom_chime_minutes(&self) -> String {
        self.custom_chime_minutes
            .iter()
            .filter(|&&m| m != 0 && m != 30)
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn format_chime_minutes(&self) -> String {
        let mins = self.effective_chime_minutes();
        if mins.is_empty() {
            "未启用整/半点报时".to_string()
        } else {
            mins.iter()
                .map(|m| format!(":{m:02}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    /// 主窗次级操作栏可读摘要（例如："整点 + 半点 · 节假日 · 2 个闹钟"）
    pub fn format_chime_summary(&self) -> String {
        let mut parts = Vec::new();
        match (self.hourly_chime_enabled, self.half_hour_chime_enabled) {
            (true, true) => parts.push("整点 + 半点报时".to_string()),
            (true, false) => parts.push("仅整点报时".to_string()),
            (false, true) => parts.push("仅半点报时".to_string()),
            (false, false) => {}
        }
        if !self.custom_chime_minutes.is_empty() {
            parts.push(format!(
                "额外分钟 {}",
                self.format_custom_chime_minutes()
            ));
        }
        if self.holiday_enabled {
            parts.push("节假日播报".to_string());
        }
        let enabled_alarms = self.alarms.iter().filter(|a| a.enabled).count();
        if enabled_alarms > 0 {
            parts.push(format!("闹钟 {enabled_alarms} 个"));
        }
        if parts.is_empty() {
            "暂无启用的报时或闹钟".to_string()
        } else {
            parts.join(" · ")
        }
    }

    pub fn next_alarm_id(&self) -> u64 {
        self.alarms.iter().map(|a| a.id).max().unwrap_or(0) + 1
    }

    /// 对整份配置做严格合法性校验与规范化
    pub fn validate(&mut self) -> Result<(), String> {
        for &m in &self.custom_chime_minutes {
            if m > 59 {
                return Err(format!("额外报时分钟 {m} 无效，应在 0–59 之间。"));
            }
        }
        self.custom_chime_minutes
            .retain(|&m| m != 0 && m != 30);
        self.custom_chime_minutes.sort_unstable();
        self.custom_chime_minutes.dedup();

        if !(1..=10).contains(&self.chime_repeat_count) {
            return Err("报时播放次数必须在 1–10 次之间。".to_string());
        }

        if self.quiet_start_hour > 23 || self.quiet_start_minute > 59 {
            return Err("静默开始时间无效（小时 0–23，分钟 0–59）。".to_string());
        }
        if self.quiet_end_hour > 23 || self.quiet_end_minute > 59 {
            return Err("静默结束时间无效（小时 0–23，分钟 0–59）。".to_string());
        }
        if self.quiet_enabled
            && self.quiet_start_hour == self.quiet_end_hour
            && self.quiet_start_minute == self.quiet_end_minute
        {
            return Err("静默开始时间与结束时间不能完全相同。".to_string());
        }

        if self.volume > 100 {
            self.volume = 100;
        }

        let mut ntp_hosts: Vec<String> = Vec::new();
        for token in self.ntp_server.split([',', '，', ';', '；']) {
            let part = token.trim();
            if part.is_empty() {
                continue;
            }
            if part.chars().any(|c| c.is_whitespace()) {
                return Err("单个 NTP 服务器地址内部不能包含空格。".to_string());
            }
            if !ntp_hosts.iter().any(|h| h.eq_ignore_ascii_case(part)) {
                ntp_hosts.push(part.to_string());
            }
        }
        if ntp_hosts.is_empty() {
            return Err("NTP 服务器地址不能为空。".to_string());
        }
        self.ntp_server = ntp_hosts.join(",");

        let update_url = crate::version::migrate_legacy_update_url(&self.update_check_url);
        let is_valid_update_target =
            update_url.starts_with("https://") || update_url.starts_with("http://");
        if !is_valid_update_target {
            return Err("更新检查地址必须以 http:// 或 https:// 开头".to_string());
        }
        self.update_check_enabled = true;
        self.update_check_url = update_url;
        self.update_github_token = self.update_github_token.trim().to_string();

        if !(1..=168).contains(&self.update_check_interval_hours) {
            return Err("更新检查周期必须在 1–168 小时之间。".to_string());
        }

        self.chime_tts_template = self.chime_tts_template.trim().to_string();
        self.holiday_custom_audio_path = self.holiday_custom_audio_path.trim().to_string();
        self.holiday_greeting_template = self.holiday_greeting_template.trim().to_string();
        self.edge_tts_voice = self.edge_tts_voice.trim().to_string();
        if self.edge_tts_voice.is_empty() {
            self.edge_tts_voice = DEFAULT_EDGE_TTS_VOICE.to_string();
        }
        self.tts_rate = self.tts_rate.clamp(-5, 5);

        let has_any_chime = !self.effective_chime_minutes().is_empty();
        if has_any_chime && !self.chime_play_sound && !self.chime_tts_enabled && !self.holiday_enabled
        {
            return Err("已启用整/半点报时，请至少勾选“内置提示音”或“语音播报真实时间”中的一项。".to_string());
        }
        if has_any_chime && self.chime_tts_enabled && self.chime_tts_template.is_empty() {
            return Err("已开启语音报时，请填写播报文字模板（例如：现在时间，{TIME}）。".to_string());
        }

        // 确保每个自定义闹钟拥有唯一正整数 ID 与合法字段
        let mut next_id = self.alarms.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        let mut seen_ids = Vec::with_capacity(self.alarms.len());
        let now = Local::now();
        for alarm in &mut self.alarms {
            if alarm.id == 0 || seen_ids.contains(&alarm.id) {
                alarm.id = next_id;
                next_id += 1;
            }
            seen_ids.push(alarm.id);
            alarm.validate()?;
            if alarm.repeat_mode == AlarmRepeatMode::IntervalDays && alarm.anchor_date.is_empty() {
                alarm.anchor_date = now.date_naive().format("%Y-%m-%d").to_string();
            }
            if alarm.repeat_mode == AlarmRepeatMode::Once && alarm.once_date.is_empty() {
                alarm.ensure_anchor_and_once_dates(now.date_naive(), now.hour(), now.minute());
            }
        }

        Ok(())
    }
}

/// 兼容旧版本 config.json 的轻量迁移动态视图（无声迁移 chime_minutes / 启用中的餐饮提醒）
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyConfigHint {
    hourly_chime_enabled: Option<bool>,
    half_hour_chime_enabled: Option<bool>,
    chime_minutes: Option<Vec<u8>>,
    alarms: Option<Vec<CustomAlarm>>,
    meal_enabled: Option<bool>,
    meal_hour: Option<u8>,
    meal_minute: Option<u8>,
    meal_repeat_count: Option<u8>,
    meal_custom_audio_path: Option<String>,
    meal_play_chime_before_custom: Option<bool>,
    meal_tts_enabled: Option<bool>,
    meal_tts_text: Option<String>,
}

fn number_to_zh(n: u32) -> String {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    if n < 10 {
        return DIGITS[n as usize].to_string();
    }
    if n == 10 {
        return "十".to_string();
    }
    if n < 20 {
        return format!("十{}", DIGITS[(n % 10) as usize]);
    }
    let tens = n / 10;
    let ones = n % 10;
    if ones == 0 {
        format!("{}十", DIGITS[tens as usize])
    } else {
        format!("{}十{}", DIGITS[tens as usize], DIGITS[ones as usize])
    }
}

fn format_spoken_hm(hour: u32, minute: u32, twelve_hour: bool) -> String {
    let h_str = if twelve_hour {
        let h12 = match hour % 12 {
            0 => 12,
            v => v,
        };
        if h12 == 2 {
            "两".to_string()
        } else {
            number_to_zh(h12)
        }
    } else if hour == 2 {
        "两点".trim_end_matches('点').to_string()
    } else {
        number_to_zh(hour)
    };

    if minute == 0 {
        format!("{h_str}点整")
    } else if minute < 10 {
        format!("{h_str}点零{}分", number_to_zh(minute))
    } else {
        format!("{h_str}点{}分", number_to_zh(minute))
    }
}

fn period_zh(hour: u32) -> &'static str {
    match hour {
        0..=5 => "凌晨",
        6..=8 => "早上",
        9..=11 => "上午",
        12..=13 => "中午",
        14..=17 => "下午",
        _ => "晚上",
    }
}

/// 将用户配置的语音播报模板中的时间占位符替换为自然中文报时文本
pub fn render_tts_template(template: &str, hour: u32, minute: u32) -> String {
    let trimmed = template.trim();
    let base = if trimmed.is_empty() {
        "现在时间，{TIME}"
    } else {
        trimmed
    };
    let period = period_zh(hour);
    let time_24 = format_spoken_hm(hour, minute, false);
    let time_12 = format!("{period}{}", format_spoken_hm(hour, minute, true));

    base.replace("{TIME}", &time_24)
        .replace("{TIME12}", &time_12)
        .replace("{PERIOD}", period)
        .replace("{HH}", &format!("{hour:02}"))
        .replace("{MM}", &format!("{minute:02}"))
        .replace("{H}", &hour.to_string())
        .replace("{M}", &minute.to_string())
}

/// 渲染闹钟语音播报文本：支持 {LABEL} 与全部时间占位符
pub fn render_alarm_tts_template(
    template: &str,
    label: &str,
    hour: u32,
    minute: u32,
) -> String {
    let expanded = template.trim().replace("{LABEL}", label.trim());
    render_tts_template(&expanded, hour, minute)
}

/// 渲染节假日问候语模板：支持 {HOLIDAY}、{GREETING}、{LUNAR} 及全部时间变量
pub fn render_holiday_template(
    template: &str,
    holiday_name: &str,
    holiday_greeting: &str,
    lunar_label: &str,
    hour: u32,
    minute: u32,
) -> String {
    let trimmed = template.trim();
    let base = if trimmed.is_empty() {
        "今天是{HOLIDAY}，{GREETING}现在时间，{TIME}"
    } else {
        trimmed
    };
    let expanded = base
        .replace("{HOLIDAY}", holiday_name)
        .replace("{GREETING}", holiday_greeting)
        .replace("{LUNAR}", lunar_label);
    render_tts_template(&expanded, hour, minute)
}

/// 配置文件路径管理器：便携优先（exe 同目录），若无写权限则自动回退到 %APPDATA%\HalfHourChime
pub struct ConfigStore {
    pub path: PathBuf,
}

impl ConfigStore {
    pub fn resolve() -> Self {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let portable_path = exe_dir.join("config.json");
                // 若已存在便携配置且可读，或者当前目录可写，则采用便携模式（方便随身携带与迁移）
                if portable_path.exists() || Self::is_dir_writable(exe_dir) {
                    return Self { path: portable_path };
                }
            }
        }

        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let dir = appdata.join("HalfHourChime");
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("config.json"),
        }
    }

    fn is_dir_writable(dir: &Path) -> bool {
        let probe = dir.join(".chime_write_probe.tmp");
        match fs::write(&probe, b"ok") {
            Ok(_) => {
                let _ = fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }

    /// 加载配置：文件不存在则生成默认配置；文件损坏则备份后安全恢复默认值；旧版配置自动无感迁移
    pub fn load_or_init(&self) -> (AppConfig, Option<String>) {
        if !self.path.exists() {
            let cfg = AppConfig::default();
            let _ = self.save(&cfg);
            return (cfg, None);
        }

        match fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<AppConfig>(&raw) {
                Ok(mut cfg) => {
                    let raw_update_url = cfg.update_check_url.clone();
                    let mut migrated = false;

                    if let Ok(hint) = serde_json::from_str::<LegacyConfigHint>(&raw) {
                        if hint.hourly_chime_enabled.is_none()
                            && hint.half_hour_chime_enabled.is_none()
                        {
                            if let Some(old_mins) = hint.chime_minutes {
                                cfg.hourly_chime_enabled = old_mins.contains(&0);
                                cfg.half_hour_chime_enabled = old_mins.contains(&30);
                                cfg.custom_chime_minutes = old_mins
                                    .into_iter()
                                    .filter(|&m| m != 0 && m != 30 && m <= 59)
                                    .collect();
                                migrated = true;
                            }
                        }

                        if hint.alarms.is_none() && hint.meal_enabled == Some(true) {
                            let text = hint
                                .meal_tts_text
                                .unwrap_or_else(|| "现在时间，{TIME}，请注意休息".to_string());
                            let custom_audio = hint.meal_custom_audio_path.unwrap_or_default();
                            let tts_enabled = hint.meal_tts_enabled.unwrap_or(false);
                            let play_before = hint.meal_play_chime_before_custom.unwrap_or(true);
                            cfg.alarms.push(CustomAlarm {
                                id: 1,
                                label: "定时提醒".to_string(),
                                enabled: true,
                                hour: hint.meal_hour.unwrap_or(12).min(23),
                                minute: hint.meal_minute.unwrap_or(0).min(59),
                                repeat_mode: AlarmRepeatMode::Daily,
                                weekdays: [true; 7],
                                interval_days: 1,
                                anchor_date: String::new(),
                                once_date: String::new(),
                                repeat_count: hint.meal_repeat_count.unwrap_or(1).clamp(1, 10),
                                play_chime_before: play_before
                                    || (!tts_enabled && custom_audio.trim().is_empty()),
                                tts_enabled,
                                tts_text: text,
                                custom_audio_path: custom_audio,
                                respect_quiet_hours: false,
                                show_alert: false,
                            });
                            migrated = true;
                        }
                    }

                    if let Err(err) = cfg.validate() {
                        let fallback = AppConfig::default();
                        self.backup_corrupted();
                        let _ = self.save(&fallback);
                        return (
                            fallback,
                            Some(format!("配置文件字段异常（{err}），已备份并恢复默认设置")),
                        );
                    }
                    if migrated || cfg.update_check_url != raw_update_url {
                        let _ = self.save(&cfg);
                    }
                    (cfg, None)
                }
                Err(_) => {
                    let fallback = AppConfig::default();
                    self.backup_corrupted();
                    let _ = self.save(&fallback);
                    (
                        fallback,
                        Some("检测到配置文件损坏，已备份原文件并以默认参数安全启动".to_string()),
                    )
                }
            },
            Err(_) => {
                let fallback = AppConfig::default();
                (
                    fallback,
                    Some("无法读取本地配置文件，当前使用默认参数运行".to_string()),
                )
            }
        }
    }

    fn backup_corrupted(&self) {
        let bak_path = self.path.with_extension("json.corrupted.bak");
        let _ = fs::rename(&self.path, bak_path);
    }

    /// 原子全量覆盖保存（临时文件写入 + 重命名，带短重试以规避 Windows 杀软/索引瞬时占用）
    pub fn save(&self, cfg: &AppConfig) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建配置目录失败：{e}"))?;
        }
        let json = serde_json::to_string_pretty(cfg)
            .map_err(|e| format!("序列化配置失败：{e}"))?;

        let tmp_path = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp_path)
                .map_err(|e| format!("写入临时配置文件失败：{e}"))?;
            use std::io::Write;
            file.write_all(json.as_bytes())
                .map_err(|e| format!("写入临时配置文件失败：{e}"))?;
            let _ = file.sync_all();
        }

        let mut last_err = None;
        for attempt in 0..5 {
            match fs::rename(&tmp_path, &self.path) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    if attempt >= 1 {
                        let _ = fs::remove_file(&self.path);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        if let Some(err) = last_err {
            return Err(format!("保存配置文件失败：{err}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chime_minutes_valid() {
        let parsed = AppConfig::parse_chime_minutes("30, 0，30, 15").unwrap();
        assert_eq!(parsed, vec![0, 15, 30]);
        let extra = AppConfig::parse_optional_extra_minutes("0, 15, 30，45").unwrap();
        assert_eq!(extra, vec![15, 45]);
        assert!(AppConfig::parse_optional_extra_minutes("").unwrap().is_empty());
    }

    #[test]
    fn test_parse_chime_minutes_invalid() {
        assert!(AppConfig::parse_optional_extra_minutes("60").is_err());
        assert!(AppConfig::parse_optional_extra_minutes("0,,30").is_err());
        assert!(AppConfig::parse_optional_extra_minutes("abc").is_err());
    }

    #[test]
    fn test_validate_quiet_same_time_rejected() {
        let mut cfg = AppConfig::default();
        cfg.quiet_enabled = true;
        cfg.quiet_start_hour = 22;
        cfg.quiet_start_minute = 0;
        cfg.quiet_end_hour = 22;
        cfg.quiet_end_minute = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_render_tts_template_natural_time() {
        assert_eq!(
            render_tts_template("现在时间，{TIME}", 12, 0),
            "现在时间，十二点整"
        );
        assert_eq!(
            render_tts_template("现在时间，{TIME}", 14, 30),
            "现在时间，十四点三十分"
        );
        assert_eq!(
            render_tts_template("现在时间，{TIME}", 2, 5),
            "现在时间，两点零五分"
        );
        assert_eq!(
            render_alarm_tts_template("现在时间，{TIME}，{LABEL}", "喝水休息", 15, 0),
            "现在时间，十五点整，喝水休息"
        );
    }

    #[test]
    fn test_validate_chime_checkboxes_and_alarms() {
        // 允许全部取消整/半点勾选（纯自定义闹钟模式）
        let mut cfg = AppConfig::default();
        cfg.hourly_chime_enabled = false;
        cfg.half_hour_chime_enabled = false;
        cfg.chime_play_sound = false;
        assert!(cfg.validate().is_ok());

        // 启用整点或半点时，提示音/语音不可同时关闭
        cfg.hourly_chime_enabled = true;
        cfg.chime_play_sound = false;
        cfg.chime_tts_enabled = false;
        assert!(cfg.validate().is_err());

        cfg.chime_tts_enabled = true;
        cfg.chime_tts_template = "   ".to_string();
        assert!(cfg.validate().is_err());

        cfg.chime_tts_template = "现在时间，{TIME}".to_string();
        assert!(cfg.validate().is_ok());

        // 闹钟校验：开启 TTS 但文本为空应拦截
        cfg.alarms.push(CustomAlarm {
            id: 1,
            label: "喝水".to_string(),
            enabled: true,
            tts_enabled: true,
            tts_text: "".to_string(),
            ..CustomAlarm::default()
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_tts_engine_and_voice_mapping() {
        assert_eq!(TtsEngine::default(), TtsEngine::Sapi5);
        assert_eq!(TtsEngine::from_index(0), TtsEngine::Sapi5);
        assert_eq!(TtsEngine::from_index(1), TtsEngine::EdgeTts);
        assert_eq!(TtsEngine::Sapi5.to_index(), 0);
        assert_eq!(TtsEngine::EdgeTts.to_index(), 1);
        assert_eq!(edge_voice_to_index(edge_voice_from_index(2)), 2);
        assert_eq!(edge_voice_from_index(99), DEFAULT_EDGE_TTS_VOICE);
    }

    #[test]
    fn test_custom_alarm_show_alert_defaults_false() {
        let alarm = CustomAlarm::default();
        assert!(!alarm.show_alert);

        let parsed: CustomAlarm = serde_json::from_str(
            r#"{"id": 2, "label": "测试", "hour": 8, "minute": 30}"#,
        )
        .unwrap();
        assert!(!parsed.show_alert);
    }
}
