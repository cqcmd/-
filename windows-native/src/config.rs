use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 支持的三套内置视觉主题
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// 暗夜曜石（面向网吧/电竞酒店夜间值守）
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

/// 语音合成引擎（本地离线 SAPI5 vs Microsoft Edge TTS 在线网络接口）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsEngine {
    /// Windows 原生 SAPI5 离线引擎（无需联网，内网隔离环境可选）
    Sapi5,
    /// Microsoft Edge TTS 在线神经网络语音（默认，网络异常或受限时自动回退至本地 SAPI5）
    #[default]
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
            TtsEngine::Sapi5 => "本地 SAPI5（离线）",
            TtsEngine::EdgeTts => "Microsoft Edge TTS（在线）",
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

/// 本地持久化配置结构（对应 SRS 4.1 数据项）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    /// 报时分钟集合（0..=59），默认 [0, 30]
    pub chime_minutes: Vec<u8>,
    /// 单次报时重复播放次数（1..=10），默认 1
    pub chime_repeat_count: u8,
    /// 半/整点报时是否播放内置提示音，默认 true
    pub chime_play_sound: bool,
    /// 半/整点报时是否启用语音播报真实时间/自定义文字，默认 false
    pub chime_tts_enabled: bool,
    /// 半/整点语音播报模板（支持 {TIME}、{TIME12}、{PERIOD}、{HH}、{MM}）
    pub chime_tts_template: String,

    /// 是否启用农历/节假日特定报时音与问候语模式（提升电竞/网吧节庆交互体验），默认 false
    pub holiday_enabled: bool,
    /// 节假日专属报时音文件路径（支持 WAV/MP3/OGG/FLAC，为空时沿用内置提示音）
    pub holiday_custom_audio_path: String,
    /// 节假日问候语播报模板（支持 {HOLIDAY}、{GREETING}、{LUNAR}、{TIME} 等变量）
    pub holiday_greeting_template: String,
    /// 自定义节假日规则文本（如 "11-14=电竞周年庆|全场畅饮福利" 或 "L01-01=新春狂欢日"）
    pub holiday_custom_rules: String,
    /// 外部节假日 JSON 数据接口或本地清单路径（支持 http(s)://、file:// 或本地/共享路径）
    pub holiday_data_source_url: String,

    /// 托盘快捷全局静音模式（开启后暂不触发定时报时与餐饮提醒播报），默认 false
    pub mute_mode: bool,
    /// 是否启用静默时段，默认 false
    pub quiet_enabled: bool,
    /// 静默开始时间（时、分），预设 00:00
    pub quiet_start_hour: u8,
    pub quiet_start_minute: u8,
    /// 静默结束时间（时、分），预设 08:00
    pub quiet_end_hour: u8,
    pub quiet_end_minute: u8,

    /// 是否启用餐饮/定时提醒，默认 false
    pub meal_enabled: bool,
    /// 餐饮提醒时间（时、分），预设 12:00
    pub meal_hour: u8,
    pub meal_minute: u8,
    /// 餐饮提醒重复播放次数（1..=10），默认 1
    pub meal_repeat_count: u8,
    /// 餐饮提醒自定义音频文件路径（为空则仅播报内置提示音）
    pub meal_custom_audio_path: String,
    /// 播报自定义餐饮音频或语音时是否先播放内置提示音，默认 true
    pub meal_play_chime_before_custom: bool,
    /// 餐饮/定时提醒是否启用自定义文字转语音播报，默认 false
    pub meal_tts_enabled: bool,
    /// 餐饮/定时提醒自定义语音播报内容（支持 {TIME}、{PERIOD} 等变量）
    pub meal_tts_text: String,

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
    /// 更新清单接口地址（JSON 格式：version, download_url, notes）
    pub update_check_url: String,
    /// 更新检查间隔（小时，1..=168），默认 6 小时
    pub update_check_interval_hours: u16,
    /// 可选 GitHub 访问令牌（PAT，仅当更新源为私有仓库时需要；公开仓库留空即可）
    pub update_github_token: String,

    /// 是否开启本地 Web 控制台（127.0.0.1:3000），默认 false（按需手动开启）
    pub web_console_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            chime_minutes: vec![0, 30],
            chime_repeat_count: 1,
            chime_play_sound: true,
            chime_tts_enabled: false,
            chime_tts_template: "现在时间，{TIME}".to_string(),
            holiday_enabled: false,
            holiday_custom_audio_path: String::new(),
            holiday_greeting_template: "今天是{HOLIDAY}，{GREETING}现在时间，{TIME}".to_string(),
            holiday_custom_rules: String::new(),
            holiday_data_source_url: String::new(),
            mute_mode: false,
            quiet_enabled: false,
            quiet_start_hour: 0,
            quiet_start_minute: 0,
            quiet_end_hour: 8,
            quiet_end_minute: 0,
            meal_enabled: false,
            meal_hour: 12,
            meal_minute: 0,
            meal_repeat_count: 1,
            meal_custom_audio_path: String::new(),
            meal_play_chime_before_custom: true,
            meal_tts_enabled: false,
            meal_tts_text: "现在时间，{TIME}，请注意休息用餐".to_string(),
            tts_engine: TtsEngine::EdgeTts,
            edge_tts_voice: DEFAULT_EDGE_TTS_VOICE.to_string(),
            tts_rate: 0,
            volume: 80,
            ntp_server: "ntp.aliyun.com".to_string(),
            auto_start: false,
            theme: ThemeMode::WarmLounge,
            update_check_enabled: true,
            update_check_url: crate::version::DEFAULT_GITHUB_RELEASE_URL.to_string(),
            update_check_interval_hours: 6,
            update_github_token: String::new(),
            web_console_enabled: false,
        }
    }
}

impl AppConfig {
    /// 解析并校验逗号/中文逗号分隔的报时分钟字符串，例如 "0, 30" 或 "0，15，30，45"
    pub fn parse_chime_minutes(input: &str) -> Result<Vec<u8>, String> {
        let normalized = input.replace('，', ",").replace('、', ",");
        let trimmed = normalized.trim();
        if trimmed.is_empty() {
            return Err("报时分钟不能为空，请至少填写一个 0–59 的整数（如 0, 30）。".to_string());
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

    pub fn format_chime_minutes(&self) -> String {
        self.chime_minutes
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// 对整份配置做严格合法性校验与规范化（FR-21 / 3.5）
    pub fn validate(&mut self) -> Result<(), String> {
        if self.chime_minutes.is_empty() {
            return Err("报时分钟集合不能为空。".to_string());
        }
        for &m in &self.chime_minutes {
            if m > 59 {
                return Err(format!("报时分钟 {m} 无效，应在 0–59 之间。"));
            }
        }
        self.chime_minutes.sort_unstable();
        self.chime_minutes.dedup();

        if !(1..=10).contains(&self.chime_repeat_count) {
            return Err("报时播放次数必须在 1–10 次之间。".to_string());
        }
        if !(1..=10).contains(&self.meal_repeat_count) {
            return Err("餐饮提醒播放次数必须在 1–10 次之间。".to_string());
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

        if self.meal_hour > 23 || self.meal_minute > 59 {
            return Err("餐饮提醒时间无效（小时 0–23，分钟 0–59）。".to_string());
        }
        if self.volume > 100 {
            self.volume = 100;
        }

        let server = self.ntp_server.trim().to_string();
        if server.is_empty() {
            return Err("NTP 服务器地址不能为空。".to_string());
        }
        if server.chars().any(|c| c.is_whitespace()) {
            return Err("NTP 服务器地址不能包含空格。".to_string());
        }
        self.ntp_server = server;

        let mut update_url = self.update_check_url.trim().to_string();
        if update_url.is_empty() {
            update_url = crate::version::DEFAULT_GITHUB_RELEASE_URL.to_string();
        }
        let is_valid_update_target = update_url.starts_with("https://")
            || update_url.starts_with("http://")
            || update_url.starts_with("file://")
            || update_url.starts_with("\\\\")
            || update_url
                .as_bytes()
                .get(1)
                .map(|&b| b == b':')
                .unwrap_or(false);
        if !is_valid_update_target {
            return Err(
                "更新检查地址须为 http(s):// 链接、局域网共享路径 (\\\\server\\...) 或本地文件路径"
                    .to_string(),
            );
        }
        self.update_check_enabled = true;
        self.update_check_url = update_url;
        self.update_github_token = self.update_github_token.trim().to_string();

        if !(1..=168).contains(&self.update_check_interval_hours) {
            return Err("更新检查周期必须在 1–168 小时之间。".to_string());
        }

        self.meal_custom_audio_path = self.meal_custom_audio_path.trim().to_string();
        self.chime_tts_template = self.chime_tts_template.trim().to_string();
        self.holiday_custom_audio_path = self.holiday_custom_audio_path.trim().to_string();
        self.holiday_greeting_template = self.holiday_greeting_template.trim().to_string();
        self.holiday_custom_rules = self.holiday_custom_rules.trim().to_string();
        self.holiday_data_source_url = self.holiday_data_source_url.trim().to_string();
        self.meal_tts_text = self.meal_tts_text.trim().to_string();
        self.edge_tts_voice = self.edge_tts_voice.trim().to_string();
        if self.edge_tts_voice.is_empty() {
            self.edge_tts_voice = DEFAULT_EDGE_TTS_VOICE.to_string();
        }
        self.tts_rate = self.tts_rate.clamp(-5, 5);

        let holiday_url = &self.holiday_data_source_url;
        let is_valid_holiday_src = holiday_url.is_empty()
            || holiday_url.starts_with("https://")
            || holiday_url.starts_with("http://")
            || holiday_url.starts_with("file://")
            || holiday_url.starts_with("\\\\")
            || holiday_url
                .as_bytes()
                .get(1)
                .map(|&b| b == b':')
                .unwrap_or(false);
        if !is_valid_holiday_src {
            return Err(
                "节假日数据源须为 http(s):// 接口地址、局域网共享路径或本地 JSON 文件路径"
                    .to_string(),
            );
        }

        if !self.chime_play_sound && !self.chime_tts_enabled && !self.holiday_enabled {
            return Err("半/整点报时须至少启用“提示音”或“语音播报”中的一项。".to_string());
        }
        if self.chime_tts_enabled && self.chime_tts_template.is_empty() {
            return Err("已开启半/整点语音播报，请填写播报文字模板（例如：现在时间，{TIME}）。".to_string());
        }
        if self.meal_enabled && self.meal_tts_enabled && self.meal_tts_text.is_empty() {
            return Err("已开启定时提醒文字转语音，请填写播报文字内容。".to_string());
        }

        Ok(())
    }
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
                // 若已存在便携配置且可读，或者当前目录可写，则采用便携模式（方便网吧批量拷盘）
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

    /// 加载配置：文件不存在则生成默认配置；文件损坏则备份后安全恢复默认值（4.2 / 5.4）
    pub fn load_or_init(&self) -> (AppConfig, Option<String>) {
        if !self.path.exists() {
            let cfg = AppConfig::default();
            let _ = self.save(&cfg);
            return (cfg, None);
        }

        match fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<AppConfig>(&raw) {
                Ok(mut cfg) => {
                    if let Err(err) = cfg.validate() {
                        let fallback = AppConfig::default();
                        self.backup_corrupted();
                        let _ = self.save(&fallback);
                        return (
                            fallback,
                            Some(format!("配置文件字段异常（{err}），已备份并恢复默认设置")),
                        );
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

    /// 原子全量覆盖保存（临时文件写入 + 重命名）
    pub fn save(&self, cfg: &AppConfig) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建配置目录失败：{e}"))?;
        }
        let json = serde_json::to_string_pretty(cfg)
            .map_err(|e| format!("序列化配置失败：{e}"))?;

        let tmp_path = self.path.with_extension("json.tmp");
        fs::write(&tmp_path, json.as_bytes())
            .map_err(|e| format!("写入临时配置文件失败：{e}"))?;

        // Windows 下若目标文件存在，fs::rename 可在现代 Rust std 下原子替换（若报错则先移除再重命名）
        if fs::rename(&tmp_path, &self.path).is_err() {
            let _ = fs::remove_file(&self.path);
            fs::rename(&tmp_path, &self.path)
                .map_err(|e| format!("保存配置文件失败：{e}"))?;
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
    }

    #[test]
    fn test_parse_chime_minutes_invalid() {
        assert!(AppConfig::parse_chime_minutes("").is_err());
        assert!(AppConfig::parse_chime_minutes("60").is_err());
        assert!(AppConfig::parse_chime_minutes("0,,30").is_err());
        assert!(AppConfig::parse_chime_minutes("abc").is_err());
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
            render_tts_template("现在是{TIME12}（{HH}:{MM}）", 14, 30),
            "现在是下午两点三十分（14:30）"
        );
    }

    #[test]
    fn test_validate_tts_rules() {
        let mut cfg = AppConfig::default();
        cfg.chime_play_sound = false;
        cfg.chime_tts_enabled = false;
        assert!(cfg.validate().is_err());

        cfg.chime_tts_enabled = true;
        cfg.chime_tts_template = "   ".to_string();
        assert!(cfg.validate().is_err());

        cfg.chime_tts_template = "现在时间，{TIME}".to_string();
        assert!(cfg.validate().is_ok());

        cfg.meal_enabled = true;
        cfg.meal_tts_enabled = true;
        cfg.meal_tts_text = "".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_tts_engine_and_voice_mapping() {
        assert_eq!(TtsEngine::from_index(0), TtsEngine::Sapi5);
        assert_eq!(TtsEngine::from_index(1), TtsEngine::EdgeTts);
        assert_eq!(TtsEngine::Sapi5.to_index(), 0);
        assert_eq!(TtsEngine::EdgeTts.to_index(), 1);
        assert_eq!(edge_voice_to_index(edge_voice_from_index(2)), 2);
        assert_eq!(edge_voice_from_index(99), DEFAULT_EDGE_TTS_VOICE);
    }
}
