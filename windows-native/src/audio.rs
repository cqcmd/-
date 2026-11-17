use crate::config::{render_tts_template, AppConfig, TtsEngine, DEFAULT_EDGE_TTS_VOICE};
use crate::win_sys;
use chrono::{DateTime, Local, Timelike, Utc};
use rodio::{Decoder, OutputStream, Sink, Source};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::Message;

/// 编译期嵌入可执行文件内部的提示音资源（保证单文件零外置依赖）
static CHIME_WAV_BYTES: &[u8] = include_bytes!("../assets/chime.wav");
static MEAL_WAV_BYTES: &[u8] = include_bytes!("../assets/meal.wav");

const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const EDGE_SEC_MS_GEC_VERSION: &str = "1-130.0.2849.68";
const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0";
const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
const WIN_EPOCH_SECS: u64 = 11_644_473_600;
const EDGE_TTS_MAX_CACHE_ENTRIES: usize = 6;
const EDGE_TTS_MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;
const EDGE_TTS_MAX_AUDIO_BYTES: usize = 4 * 1024 * 1024;

/// 提示音与自定义音频默认淡入/淡出时长（避免网吧公放或耳机环境下起止突兀）
const AUDIO_FADE_IN_MS: u64 = 180;
const AUDIO_FADE_OUT_MS: u64 = 240;
/// 语音流默认淡入/淡出时长（短包络去爆音且不吞字头字尾）
const SPEECH_FADE_IN_MS: u64 = 90;
const SPEECH_FADE_OUT_MS: u64 = 130;
/// 打断切换旧音频时的平滑渐隐时长
const AUDIO_SWITCH_FADE_MS: u64 = 140;

/// 触发餐饮提醒时对其他应用背景音乐的压低策略（压低至原音量的 25%，最低保留 5%）
const AUDIO_DUCK_TARGET_RATIO: f32 = 0.25;
const AUDIO_DUCK_MIN_ACTIVE_VOL: f32 = 0.06;
const AUDIO_DUCK_FLOOR_VOL: f32 = 0.05;
const AUDIO_DUCK_FADE_DOWN_MS: u64 = 240;
const AUDIO_DUCK_FADE_UP_MS: u64 = 420;
const AUDIO_DUCK_CANCEL_RESTORE_MS: u64 = 140;

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
enum SequenceKind {
    Chime {
        repeat: u8,
        play_sound: bool,
        custom_audio_path: Option<String>,
        tts_text: Option<String>,
        tts_engine: TtsEngine,
        edge_voice: String,
        tts_rate: i8,
    },
    Meal {
        repeat: u8,
        custom_path: Option<String>,
        chime_with_custom: bool,
        tts_text: Option<String>,
        tts_engine: TtsEngine,
        edge_voice: String,
        tts_rate: i8,
    },
}

#[derive(Debug)]
enum AudioCommand {
    PlayChimeWav {
        volume: u8,
        custom_path: Option<String>,
        reply: SyncSender<Option<Arc<Sink>>>,
    },
    PlayMealAudio {
        volume: u8,
        include_built_in: bool,
        custom_path: Option<String>,
        reply: SyncSender<Option<Arc<Sink>>>,
    },
    PlayMemoryAudio {
        volume: u8,
        bytes: Vec<u8>,
        reply: SyncSender<Option<Arc<Sink>>>,
    },
    StopActive,
    SetLiveVolume(u8),
    TrimIdle,
    Shutdown,
}

struct SequenceJob {
    gen_id: u64,
    kind: SequenceKind,
}

#[derive(Default)]
struct EdgeTtsCache {
    entries: VecDeque<(String, Vec<u8>)>,
    total_bytes: usize,
}

impl EdgeTtsCache {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, bytes)| bytes.clone())
    }

    fn insert(&mut self, key: String, bytes: Vec<u8>) {
        if bytes.is_empty() || bytes.len() > EDGE_TTS_MAX_CACHE_BYTES / 2 {
            return;
        }
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            if let Some((_, removed)) = self.entries.remove(pos) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.len());
            }
        }
        while self.entries.len() >= EDGE_TTS_MAX_CACHE_ENTRIES
            || self.total_bytes + bytes.len() > EDGE_TTS_MAX_CACHE_BYTES
        {
            if let Some((_, oldest)) = self.entries.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(oldest.len());
            } else {
                break;
            }
        }
        self.total_bytes += bytes.len();
        self.entries.push_back((key, bytes));
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }
}

/// 异步音频与双引擎语音合成句柄（支持 SAPI5 离线与 Microsoft Edge TTS 在线切换，断网自动回退）
#[derive(Clone)]
pub struct AudioEngine {
    audio_tx: Sender<AudioCommand>,
    seq_tx: Sender<SequenceJob>,
    gen_counter: Arc<AtomicU64>,
    live_volume: Arc<AtomicU8>,
    tts_cache: Arc<Mutex<EdgeTtsCache>>,
}

/// 平滑余弦 S 型曲线：t=0 时为 0.0，t=1 时为 1.0，两端一阶导数为 0，消除淡入/淡出切换爆音
#[inline]
fn smooth_cosine_ramp(t: f32) -> f32 {
    let x = t.clamp(0.0, 1.0);
    0.5 * (1.0 - (PI * x).cos())
}

/// 根据背景音频会话原始音量计算餐饮提醒期间的目标压低音量；若本就接近静音则返回 None 跳过处理
pub fn compute_ducked_volume(original_volume: f32) -> Option<f32> {
    if !original_volume.is_finite() || original_volume <= AUDIO_DUCK_MIN_ACTIVE_VOL {
        return None;
    }
    let clamped = original_volume.clamp(0.0, 1.0);
    let target = (clamped * AUDIO_DUCK_TARGET_RATIO)
        .max(AUDIO_DUCK_FLOOR_VOL)
        .min(clamped);
    if clamped - target < 0.01 {
        None
    } else {
        Some(target)
    }
}

/// 采样级淡入淡出音频包络包装器：利用轻量尾音预读缓冲支持任意格式（WAV/MP3/FLAC/OGG）的完整淡入与淡出
struct FadeEnvelopeSource<S>
where
    S: Source<Item = f32>,
{
    inner: S,
    channels: u16,
    sample_rate: u32,
    total_duration: Option<Duration>,
    fade_in_samples: usize,
    fade_out_samples: usize,
    effective_fade_in_samples: usize,
    effective_fade_out_samples: usize,
    played_samples: usize,
    lookahead: VecDeque<f32>,
    eof: bool,
}

impl<S> FadeEnvelopeSource<S>
where
    S: Source<Item = f32>,
{
    fn new(inner: S, fade_in: Duration, fade_out: Duration) -> Self {
        let channels = inner.channels().max(1);
        let sample_rate = inner.sample_rate().max(1);
        let total_duration = inner.total_duration();
        let ch = usize::from(channels);
        let samples_per_sec = u64::from(sample_rate) * u64::from(channels);

        let align_to_frame = |ms: u128| -> usize {
            let raw = ((u128::from(samples_per_sec) * ms) / 1000) as usize;
            (raw / ch) * ch
        };

        let fade_in_samples = align_to_frame(fade_in.as_millis());
        let fade_out_samples = align_to_frame(fade_out.as_millis());

        Self {
            inner,
            channels,
            sample_rate,
            total_duration,
            fade_in_samples,
            fade_out_samples,
            effective_fade_in_samples: fade_in_samples,
            effective_fade_out_samples: fade_out_samples,
            played_samples: 0,
            lookahead: VecDeque::with_capacity(fade_out_samples.max(1)),
            eof: false,
        }
    }
}

impl<S> Iterator for FadeEnvelopeSource<S>
where
    S: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let target_buf = self.fade_out_samples.max(1);
        while !self.eof && self.lookahead.len() < target_buf {
            match self.inner.next() {
                Some(sample) => self.lookahead.push_back(sample),
                None => {
                    self.eof = true;
                    // 若整段音频长度短于淡入+淡出总和，则在首帧处按总长度对半收敛，防止整段声音被压低
                    if self.played_samples == 0
                        && self.lookahead.len() < self.fade_in_samples + self.fade_out_samples
                    {
                        let ch = usize::from(self.channels);
                        let total_frames = self.lookahead.len() / ch;
                        let half_samples = (total_frames / 2) * ch;
                        self.effective_fade_in_samples = self.fade_in_samples.min(half_samples);
                        self.effective_fade_out_samples = self.fade_out_samples.min(half_samples);
                    }
                    break;
                }
            }
        }

        let sample = self.lookahead.pop_front()?;
        let ch = usize::from(self.channels);

        let fade_in_gain = if self.effective_fade_in_samples > 0
            && self.played_samples < self.effective_fade_in_samples
        {
            let frame_idx = self.played_samples / ch;
            let in_frames = (self.effective_fade_in_samples / ch).max(1);
            smooth_cosine_ramp((frame_idx as f32) / (in_frames as f32))
        } else {
            1.0
        };

        let fade_out_gain = if self.eof
            && self.effective_fade_out_samples > 0
            && self.lookahead.len() < self.effective_fade_out_samples
        {
            let rem_frames = self.lookahead.len() / ch;
            let out_frames = (self.effective_fade_out_samples / ch).max(1);
            smooth_cosine_ramp((rem_frames as f32) / (out_frames as f32))
        } else {
            1.0
        };

        self.played_samples = self.played_samples.saturating_add(1);
        Some(sample * fade_in_gain * fade_out_gain)
    }
}

impl<S> Source for FadeEnvelopeSource<S>
where
    S: Source<Item = f32>,
{
    #[inline]
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    #[inline]
    fn channels(&self) -> u16 {
        self.channels
    }

    #[inline]
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

/// 平滑渐隐后停止当前正在播放的 Sink，避免报时切换或重复触发时出现突兀切断与爆音
fn fade_out_and_stop_sink(sink: Arc<Sink>, fade_ms: u64) {
    if sink.empty() || sink.is_paused() {
        sink.stop();
        return;
    }
    let start_vol = sink.volume().clamp(0.0, 1.0);
    if start_vol <= 0.01 || fade_ms == 0 {
        sink.stop();
        return;
    }
    let steps = 8u64;
    let step_dur = Duration::from_millis((fade_ms / steps).max(10));
    for step in 1..=steps {
        if sink.empty() {
            break;
        }
        let ratio = 1.0 - (step as f32 / steps as f32);
        sink.set_volume(start_vol * smooth_cosine_ramp(ratio));
        thread::sleep(step_dur);
    }
    sink.stop();
}

/// Windows WASAPI 背景音乐自动压低（Audio Ducking）RAII 守卫：
/// 触发餐饮提醒时自动识别并平滑压低其他活跃应用音频会话，播报结束或被中断时平滑恢复原音量
struct BackgroundAudioDuckGuard {
    gen_id: u64,
    gen_seq: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    sessions: Vec<windows_duck::DuckedSession>,
    #[cfg(target_os = "windows")]
    com_initialized: bool,
}

impl BackgroundAudioDuckGuard {
    fn acquire(gen_id: u64, gen_seq: Arc<AtomicU64>) -> Self {
        #[cfg(target_os = "windows")]
        {
            let (sessions, com_initialized) =
                windows_duck::acquire_and_duck_sessions(gen_id, &gen_seq);
            Self {
                gen_id,
                gen_seq,
                sessions,
                com_initialized,
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Self { gen_id, gen_seq }
        }
    }
}

impl Drop for BackgroundAudioDuckGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            let cancelled = self.gen_seq.load(Ordering::SeqCst) != self.gen_id;
            windows_duck::restore_and_release_sessions(
                &mut self.sessions,
                cancelled,
                self.com_initialized,
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (&self.gen_id, &self.gen_seq);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_duck {
    use super::*;
    use windows::core::Interface;
    use windows::Win32::Foundation::S_OK;
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, AudioSessionStateActive, IAudioSessionControl2,
        IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };

    pub(super) struct DuckedSession {
        volume_ctl: ISimpleAudioVolume,
        original_volume: f32,
        ducked_volume: f32,
    }

    pub(super) fn acquire_and_duck_sessions(
        gen_id: u64,
        gen_seq: &AtomicU64,
    ) -> (Vec<DuckedSession>, bool) {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let com_initialized = hr.is_ok();

            let mut collected: Vec<DuckedSession> = Vec::new();
            let current_pid = std::process::id();

            let enumerate_res: windows::core::Result<()> = (|| {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                let session_enum = manager.GetSessionEnumerator()?;
                let count = session_enum.GetCount()?;

                for idx in 0..count {
                    let Ok(ctrl) = session_enum.GetSession(idx) else {
                        continue;
                    };
                    let Ok(ctrl2) = ctrl.cast::<IAudioSessionControl2>() else {
                        continue;
                    };
                    // 跳过 Windows 系统提示音会话与本程序自身音频会话
                    if ctrl2.IsSystemSoundsSession() == S_OK {
                        continue;
                    }
                    let Ok(state) = ctrl2.GetState() else {
                        continue;
                    };
                    if state != AudioSessionStateActive {
                        continue;
                    }
                    if let Ok(pid) = ctrl2.GetProcessId() {
                        if pid == current_pid {
                            continue;
                        }
                    }
                    let Ok(volume_ctl) = ctrl.cast::<ISimpleAudioVolume>() else {
                        continue;
                    };
                    if let Ok(muted) = volume_ctl.GetMute() {
                        if muted.as_bool() {
                            continue;
                        }
                    }
                    let Ok(orig_vol) = volume_ctl.GetMasterVolume() else {
                        continue;
                    };
                    if let Some(target_vol) = compute_ducked_volume(orig_vol) {
                        collected.push(DuckedSession {
                            volume_ctl,
                            original_volume: orig_vol,
                            ducked_volume: target_vol,
                        });
                    }
                }
                Ok(())
            })();

            if let Err(err) = enumerate_res {
                win_sys::log_debug(
                    "audio",
                    &format!("WASAPI session enumeration skipped for ducking: {err}"),
                );
                collected.clear();
                if com_initialized {
                    CoUninitialize();
                }
                return (collected, false);
            }

            if collected.is_empty() {
                if com_initialized {
                    CoUninitialize();
                }
                return (collected, false);
            }

            // 平滑压低背景音乐音量
            let steps = 8u64;
            let step_dur = Duration::from_millis((AUDIO_DUCK_FADE_DOWN_MS / steps).max(12));
            for step in 1..=steps {
                if gen_seq.load(Ordering::SeqCst) != gen_id {
                    break;
                }
                let k = smooth_cosine_ramp(step as f32 / steps as f32);
                for session in &collected {
                    let level = session.original_volume
                        + (session.ducked_volume - session.original_volume) * k;
                    let _ = session.volume_ctl.SetMasterVolume(level, std::ptr::null());
                }
                thread::sleep(step_dur);
            }

            win_sys::log_info(
                "audio",
                &format!(
                    "ducked {} active background audio session(s) for meal reminder",
                    collected.len()
                ),
            );
            (collected, com_initialized)
        }
    }

    pub(super) fn restore_and_release_sessions(
        sessions: &mut Vec<DuckedSession>,
        cancelled: bool,
        com_initialized: bool,
    ) {
        unsafe {
            if !sessions.is_empty() {
                // 若用户在提醒期间手动大幅调节过该软件音量，则尊重用户选择不强行覆盖
                let to_restore: Vec<&DuckedSession> = sessions
                    .iter()
                    .filter(|s| {
                        s.volume_ctl
                            .GetMasterVolume()
                            .map(|cur| (cur - s.ducked_volume).abs() <= 0.15)
                            .unwrap_or(false)
                    })
                    .collect();

                if !to_restore.is_empty() {
                    let total_ms = if cancelled {
                        AUDIO_DUCK_CANCEL_RESTORE_MS
                    } else {
                        AUDIO_DUCK_FADE_UP_MS
                    };
                    let steps = if cancelled { 5u64 } else { 10u64 };
                    let step_dur = Duration::from_millis((total_ms / steps).max(12));
                    for step in 1..=steps {
                        let k = smooth_cosine_ramp(step as f32 / steps as f32);
                        for session in &to_restore {
                            let level = session.ducked_volume
                                + (session.original_volume - session.ducked_volume) * k;
                            let _ = session.volume_ctl.SetMasterVolume(level, std::ptr::null());
                        }
                        thread::sleep(step_dur);
                    }
                }

                win_sys::log_info(
                    "audio",
                    &format!(
                        "restored background audio volume for {} session(s)",
                        sessions.len()
                    ),
                );
                // 必须在 CoUninitialize 之前释放所有 COM 接口指针
                sessions.clear();
            }

            if com_initialized {
                CoUninitialize();
            }
        }
    }
}

fn append_custom_audio(sink: &Sink, path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    let p = Path::new(trimmed);
    match File::open(p) {
        Ok(file) => match Decoder::new(BufReader::new(file)) {
            Ok(source) => {
                let faded = FadeEnvelopeSource::new(
                    source.convert_samples::<f32>(),
                    Duration::from_millis(AUDIO_FADE_IN_MS),
                    Duration::from_millis(AUDIO_FADE_OUT_MS),
                );
                sink.append(faded);
                true
            }
            Err(err) => {
                win_sys::log_warn(
                    "audio",
                    &format!("decode custom meal audio \"{trimmed}\" failed: {err}"),
                );
                false
            }
        },
        Err(err) => {
            win_sys::log_warn(
                "audio",
                &format!("open custom meal audio \"{trimmed}\" failed: {err}"),
            );
            false
        }
    }
}

fn wait_sink_with_cancel(sink: &Arc<Sink>, gen_id: u64, cur_gen: &AtomicU64) -> bool {
    while !sink.empty() {
        if cur_gen.load(Ordering::SeqCst) != gen_id {
            fade_out_and_stop_sink(Arc::clone(sink), AUDIO_SWITCH_FADE_MS);
            return false;
        }
        thread::sleep(Duration::from_millis(35));
    }
    cur_gen.load(Ordering::SeqCst) == gen_id
}

fn generate_request_id_hex() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = REQ_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    format!("{:016x}{:016x}", nanos ^ pid.rotate_left(17), seq.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// 计算 Microsoft Edge Read Aloud WebSocket 接口所需的 Sec-MS-GEC 校验令牌
pub fn generate_sec_ms_gec(unix_secs: u64) -> String {
    let win_secs = unix_secs.saturating_add(WIN_EPOCH_SECS);
    let rounded_secs = win_secs - (win_secs % 300);
    let ticks = u128::from(rounded_secs) * 10_000_000;
    let payload = format!("{ticks}{EDGE_TRUSTED_CLIENT_TOKEN}");
    let digest = Sha256::digest(payload.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02X}");
    }
    hex
}

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 构造 Edge TTS 所需的 SSML 文本与语速参数
pub fn build_edge_ssml(text: &str, voice: &str, rate: i8) -> String {
    let safe_voice = if voice.trim().is_empty() {
        DEFAULT_EDGE_TTS_VOICE
    } else {
        voice.trim()
    };
    let rate_pct = i32::from(rate.clamp(-5, 5)) * 10;
    let rate_str = if rate_pct >= 0 {
        format!("+{rate_pct}%")
    } else {
        format!("{rate_pct}%")
    };
    let escaped = escape_xml(text.trim());
    format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='zh-CN'><voice name='{safe_voice}'><prosody pitch='+0Hz' rate='{rate_str}' volume='+0%'>{escaped}</prosody></voice></speak>"
    )
}

/// 从 Edge TTS 二进制 WebSocket 帧中解析出音频数据分片（跳过长度前缀与 UTF-8 头部）
pub fn extract_audio_chunk_from_binary_frame(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < 2 {
        return None;
    }
    let header_len = u16::from_be_bytes([frame[0], frame[1]]) as usize;
    let payload_offset = 2usize.checked_add(header_len)?;
    if payload_offset > frame.len() {
        return None;
    }
    let header = std::str::from_utf8(&frame[2..payload_offset]).ok()?;
    if header.contains("Path:audio") {
        Some(&frame[payload_offset..])
    } else {
        None
    }
}

/// 通过 Microsoft Edge TTS 在线 WebSocket 接口合成 MP3 字节流（带连接/读写超时保护）
fn synthesize_edge_tts_mp3(text: &str, voice: &str, rate: i8) -> Result<Vec<u8>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("empty tts text".to_string());
    }

    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let conn_id = generate_request_id_hex();
    let sec_ms_gec = generate_sec_ms_gec(unix_secs);
    let url = format!(
        "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken={EDGE_TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={sec_ms_gec}&Sec-MS-GEC-Version={EDGE_SEC_MS_GEC_VERSION}&ConnectionId={conn_id}"
    );

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("构造 Edge TTS 请求失败：{e}"))?;
    {
        let headers = request.headers_mut();
        headers.insert("Pragma", HeaderValue::from_static("no-cache"));
        headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        headers.insert("Origin", HeaderValue::from_static(EDGE_ORIGIN));
        headers.insert("User-Agent", HeaderValue::from_static(EDGE_USER_AGENT));
        headers.insert(
            "Accept-Language",
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );
    }

    let addrs = ("speech.platform.bing.com", 443)
        .to_socket_addrs()
        .map_err(|e| format!("DNS 解析 speech.platform.bing.com 失败：{e}"))?;

    let mut tcp_stream: Option<TcpStream> = None;
    let mut last_err = String::from("无可用网络地址");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(s) => {
                tcp_stream = Some(s);
                break;
            }
            Err(err) => {
                last_err = err.to_string();
            }
        }
    }
    let stream = tcp_stream.ok_or_else(|| format!("连接 Edge TTS 服务器超时或被拦截：{last_err}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let (mut socket, _resp) = tungstenite::client_tls(request, stream)
        .map_err(|e| format!("Edge TTS WebSocket TLS 握手失败：{e}"))?;

    let timestamp = Utc::now()
        .format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string();

    let speech_config_msg = format!(
        "X-Timestamp:{timestamp}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}"
    );
    socket
        .send(Message::Text(speech_config_msg.into()))
        .map_err(|e| format!("发送 speech.config 失败：{e}"))?;

    let ssml = build_edge_ssml(trimmed, voice, rate);
    let ssml_msg = format!(
        "X-RequestId:{conn_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{timestamp}Z\r\nPath:ssml\r\n\r\n{ssml}"
    );
    socket
        .send(Message::Text(ssml_msg.into()))
        .map_err(|e| format!("发送 SSML 请求失败：{e}"))?;

    let mut audio_bytes: Vec<u8> = Vec::with_capacity(32 * 1024);
    loop {
        match socket.read() {
            Ok(Message::Binary(bin)) => {
                if let Some(chunk) = extract_audio_chunk_from_binary_frame(&bin) {
                    if audio_bytes.len() + chunk.len() <= EDGE_TTS_MAX_AUDIO_BYTES {
                        audio_bytes.extend_from_slice(chunk);
                    }
                }
            }
            Ok(Message::Text(txt)) => {
                if txt.contains("Path:turn.end") {
                    break;
                }
            }
            Ok(Message::Ping(payload)) => {
                let _ = socket.send(Message::Pong(payload));
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(err) => {
                if audio_bytes.is_empty() {
                    return Err(format!("接收 Edge TTS 音频流失败：{err}"));
                }
                break;
            }
        }
    }

    let _ = socket.close(None);
    if audio_bytes.is_empty() {
        return Err("Edge TTS 返回空音频流".to_string());
    }
    Ok(audio_bytes)
}

#[allow(clippy::too_many_arguments)]
fn speak_with_engine(
    text: &str,
    volume: u8,
    tts_engine: TtsEngine,
    edge_voice: &str,
    tts_rate: i8,
    gen_id: u64,
    gen_seq: &AtomicU64,
    audio_tx_seq: &Sender<AudioCommand>,
    tts_cache: &Arc<Mutex<EdgeTtsCache>>,
) -> bool {
    if gen_seq.load(Ordering::SeqCst) != gen_id {
        return false;
    }

    win_sys::log_debug(
        "tts",
        &format!("speaking via {:?} rate={tts_rate} vol={volume}: \"{text}\"", tts_engine),
    );

    if tts_engine == TtsEngine::EdgeTts {
        let voice = if edge_voice.trim().is_empty() {
            DEFAULT_EDGE_TTS_VOICE
        } else {
            edge_voice.trim()
        };
        let cache_key = format!("{voice}|{tts_rate}|{text}");
        let cached = tts_cache.lock().ok().and_then(|c| c.get(&cache_key));

        let mp3_result = match cached {
            Some(bytes) => Ok(bytes),
            None => synthesize_edge_tts_mp3(text, voice, tts_rate).map(|bytes| {
                if let Ok(mut c) = tts_cache.lock() {
                    c.insert(cache_key, bytes.clone());
                }
                bytes
            }),
        };

        if gen_seq.load(Ordering::SeqCst) != gen_id {
            return false;
        }

        match mp3_result {
            Ok(bytes) => {
                let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                let _ = audio_tx_seq.send(AudioCommand::PlayMemoryAudio {
                    volume,
                    bytes,
                    reply: reply_tx,
                });
                if let Ok(Some(sink)) = reply_rx.recv() {
                    return wait_sink_with_cancel(&sink, gen_id, gen_seq);
                }
                win_sys::log_warn(
                    "tts",
                    "Edge TTS MP3 decoder unavailable, falling back to local SAPI5",
                );
            }
            Err(err) => {
                win_sys::log_warn(
                    "tts",
                    &format!("Edge TTS unavailable ({err}), falling back to local SAPI5"),
                );
            }
        }
    }

    if gen_seq.load(Ordering::SeqCst) != gen_id {
        return false;
    }
    if let Err(err) = win_sys::speak_text_sapi(text, volume, tts_rate) {
        win_sys::log_warn("tts", &err);
    }
    gen_seq.load(Ordering::SeqCst) == gen_id
}

impl AudioEngine {
    pub fn start() -> Self {
        let (audio_tx, audio_rx) = mpsc::channel::<AudioCommand>();
        let (seq_tx, seq_rx) = mpsc::channel::<SequenceJob>();
        let gen_counter = Arc::new(AtomicU64::new(0));
        let live_volume = Arc::new(AtomicU8::new(80));
        let tts_cache = Arc::new(Mutex::new(EdgeTtsCache::default()));

        // 1. 底层 rodio 音频线程（负责 OutputStream、提示音/自定义音频、Edge TTS MP3 播放与实时音量）
        thread::Builder::new()
            .name("audio-worker".to_string())
            .spawn(move || {
                let stream_pair = match OutputStream::try_default() {
                    Ok(pair) => Some(pair),
                    Err(err) => {
                        win_sys::log_error(
                            "audio",
                            &format!("OutputStream::try_default failed (no active playback device): {err}"),
                        );
                        None
                    }
                };
                let mut active_sink: Option<Arc<Sink>> = None;

                while let Ok(cmd) = audio_rx.recv() {
                    match cmd {
                        AudioCommand::SetLiveVolume(vol) => {
                            let vol_f = (vol.min(100) as f32) / 100.0;
                            if let Some(sink) = &active_sink {
                                sink.set_volume(vol_f);
                            }
                        }
                        AudioCommand::StopActive => {
                            if let Some(prev) = active_sink.take() {
                                fade_out_and_stop_sink(prev, AUDIO_SWITCH_FADE_MS);
                            }
                        }
                        AudioCommand::PlayChimeWav {
                            volume,
                            custom_path,
                            reply,
                        } => {
                            let vol_f = (volume.min(100) as f32) / 100.0;
                            let created = if let Some((_, handle)) = &stream_pair {
                                if let Some(prev) = active_sink.take() {
                                    fade_out_and_stop_sink(prev, AUDIO_SWITCH_FADE_MS);
                                }
                                if let Ok(sink) = Sink::try_new(handle) {
                                    sink.set_volume(vol_f);
                                    let custom_ok = custom_path
                                        .as_deref()
                                        .is_some_and(|p| append_custom_audio(&sink, p));
                                    if !custom_ok {
                                        if let Ok(source) = Decoder::new(Cursor::new(CHIME_WAV_BYTES))
                                        {
                                            let faded = FadeEnvelopeSource::new(
                                                source.convert_samples::<f32>(),
                                                Duration::from_millis(AUDIO_FADE_IN_MS),
                                                Duration::from_millis(AUDIO_FADE_OUT_MS),
                                            );
                                            sink.append(faded);
                                        }
                                    }
                                    let arc_sink = Arc::new(sink);
                                    active_sink = Some(Arc::clone(&arc_sink));
                                    Some(arc_sink)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            let _ = reply.send(created);
                        }
                        AudioCommand::PlayMealAudio {
                            volume,
                            include_built_in,
                            custom_path,
                            reply,
                        } => {
                            let vol_f = (volume.min(100) as f32) / 100.0;
                            let created = if let Some((_, handle)) = &stream_pair {
                                if let Some(prev) = active_sink.take() {
                                    fade_out_and_stop_sink(prev, AUDIO_SWITCH_FADE_MS);
                                }
                                if let Ok(sink) = Sink::try_new(handle) {
                                    sink.set_volume(vol_f);
                                    let custom_trimmed = custom_path
                                        .as_deref()
                                        .map(str::trim)
                                        .filter(|s| !s.is_empty());

                                    let append_builtin_faded = |target_sink: &Sink| {
                                        if let Ok(source) =
                                            Decoder::new(Cursor::new(MEAL_WAV_BYTES))
                                        {
                                            let faded = FadeEnvelopeSource::new(
                                                source.convert_samples::<f32>(),
                                                Duration::from_millis(AUDIO_FADE_IN_MS),
                                                Duration::from_millis(AUDIO_FADE_OUT_MS),
                                            );
                                            target_sink.append(faded);
                                        }
                                    };

                                    match custom_trimmed {
                                        Some(path_str) => {
                                            if include_built_in {
                                                append_builtin_faded(&sink);
                                            }
                                            if !append_custom_audio(&sink, path_str)
                                                && !include_built_in
                                            {
                                                append_builtin_faded(&sink);
                                            }
                                        }
                                        None => {
                                            if include_built_in {
                                                append_builtin_faded(&sink);
                                            }
                                        }
                                    }
                                    let arc_sink = Arc::new(sink);
                                    active_sink = Some(Arc::clone(&arc_sink));
                                    Some(arc_sink)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            let _ = reply.send(created);
                        }
                        AudioCommand::PlayMemoryAudio {
                            volume,
                            bytes,
                            reply,
                        } => {
                            let vol_f = (volume.min(100) as f32) / 100.0;
                            let created = if let Some((_, handle)) = &stream_pair {
                                if let Some(prev) = active_sink.take() {
                                    fade_out_and_stop_sink(prev, AUDIO_SWITCH_FADE_MS);
                                }
                                match (Sink::try_new(handle), Decoder::new(Cursor::new(bytes))) {
                                    (Ok(sink), Ok(source)) => {
                                        sink.set_volume(vol_f);
                                        let faded = FadeEnvelopeSource::new(
                                            source.convert_samples::<f32>(),
                                            Duration::from_millis(SPEECH_FADE_IN_MS),
                                            Duration::from_millis(SPEECH_FADE_OUT_MS),
                                        );
                                        sink.append(faded);
                                        let arc_sink = Arc::new(sink);
                                        active_sink = Some(Arc::clone(&arc_sink));
                                        Some(arc_sink)
                                    }
                                    _ => None,
                                }
                            } else {
                                None
                            };
                            let _ = reply.send(created);
                        }
                        AudioCommand::TrimIdle => {
                            if let Some(sink) = active_sink.as_ref() {
                                if sink.empty() {
                                    active_sink = None;
                                }
                            }
                        }
                        AudioCommand::Shutdown => {
                            if let Some(sink) = active_sink.take() {
                                fade_out_and_stop_sink(sink, 80);
                            }
                            break;
                        }
                    }
                }
            })
            .expect("Failed to spawn audio worker thread");

        // 2. 编排线程（保证提示音、自定义音频与 SAPI5 / Edge TTS 语音按顺序串行播放，不叠音、不阻塞 UI）
        let audio_tx_seq = audio_tx.clone();
        let gen_seq = Arc::clone(&gen_counter);
        let vol_seq = Arc::clone(&live_volume);
        let cache_seq = Arc::clone(&tts_cache);
        thread::Builder::new()
            .name("audio-sequence-worker".to_string())
            .spawn(move || {
                while let Ok(job) = seq_rx.recv() {
                    if gen_seq.load(Ordering::SeqCst) != job.gen_id {
                        continue;
                    }

                    match job.kind {
                        SequenceKind::Chime {
                            repeat,
                            play_sound,
                            custom_audio_path,
                            tts_text,
                            tts_engine,
                            edge_voice,
                            tts_rate,
                        } => {
                            let spoken = tts_text
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty());
                            let has_custom_holiday_audio = custom_audio_path
                                .as_deref()
                                .map(str::trim)
                                .is_some_and(|s| !s.is_empty());

                            for _ in 0..repeat.max(1) {
                                if gen_seq.load(Ordering::SeqCst) != job.gen_id {
                                    break;
                                }
                                if play_sound || has_custom_holiday_audio || spoken.is_none() {
                                    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                                    let cur_vol = vol_seq.load(Ordering::Relaxed);
                                    let _ = audio_tx_seq.send(AudioCommand::PlayChimeWav {
                                        volume: cur_vol,
                                        custom_path: custom_audio_path.clone(),
                                        reply: reply_tx,
                                    });
                                    if let Ok(Some(sink)) = reply_rx.recv() {
                                        if !wait_sink_with_cancel(&sink, job.gen_id, &gen_seq) {
                                            break;
                                        }
                                    }
                                }
                                if let Some(text) = spoken {
                                    let cur_vol = vol_seq.load(Ordering::Relaxed);
                                    if !speak_with_engine(
                                        text,
                                        cur_vol,
                                        tts_engine,
                                        &edge_voice,
                                        tts_rate,
                                        job.gen_id,
                                        &gen_seq,
                                        &audio_tx_seq,
                                        &cache_seq,
                                    ) {
                                        break;
                                    }
                                }
                            }
                        }
                        SequenceKind::Meal {
                            repeat,
                            custom_path,
                            chime_with_custom,
                            tts_text,
                            tts_engine,
                            edge_voice,
                            tts_rate,
                        } => {
                            // 触发餐饮提醒时自动平滑压低其他后台音乐/媒体音量，播报结束或被中断时自动恢复
                            let _duck_guard =
                                BackgroundAudioDuckGuard::acquire(job.gen_id, Arc::clone(&gen_seq));

                            let spoken = tts_text
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty());
                            let has_custom = custom_path
                                .as_deref()
                                .map(str::trim)
                                .is_some_and(|s| !s.is_empty());
                            let include_built_in =
                                chime_with_custom || (!has_custom && spoken.is_none());

                            for _ in 0..repeat.max(1) {
                                if gen_seq.load(Ordering::SeqCst) != job.gen_id {
                                    break;
                                }
                                if include_built_in || has_custom {
                                    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
                                    let cur_vol = vol_seq.load(Ordering::Relaxed);
                                    let _ = audio_tx_seq.send(AudioCommand::PlayMealAudio {
                                        volume: cur_vol,
                                        include_built_in,
                                        custom_path: custom_path.clone(),
                                        reply: reply_tx,
                                    });
                                    if let Ok(Some(sink)) = reply_rx.recv() {
                                        if !wait_sink_with_cancel(&sink, job.gen_id, &gen_seq) {
                                            break;
                                        }
                                    }
                                }
                                if let Some(text) = spoken {
                                    let cur_vol = vol_seq.load(Ordering::Relaxed);
                                    if !speak_with_engine(
                                        text,
                                        cur_vol,
                                        tts_engine,
                                        &edge_voice,
                                        tts_rate,
                                        job.gen_id,
                                        &gen_seq,
                                        &audio_tx_seq,
                                        &cache_seq,
                                    ) {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn audio sequence worker thread");

        Self {
            audio_tx,
            seq_tx,
            gen_counter,
            live_volume,
            tts_cache,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn play_chime(
        &self,
        repeat: u8,
        volume: u8,
        play_sound: bool,
        custom_audio_path: Option<String>,
        tts_text: Option<String>,
        tts_engine: TtsEngine,
        edge_voice: String,
        tts_rate: i8,
    ) {
        self.live_volume.store(volume.min(100), Ordering::Relaxed);
        let gen_id = self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.audio_tx.send(AudioCommand::StopActive);
        let _ = self.seq_tx.send(SequenceJob {
            gen_id,
            kind: SequenceKind::Chime {
                repeat,
                play_sound,
                custom_audio_path,
                tts_text,
                tts_engine,
                edge_voice,
                tts_rate,
            },
        });
    }

    /// 依据配置开关（`chime_tts_enabled`）在半/整点报时时自动调用 TTS 引擎播报当前真实系统时间
    pub fn play_chime_with_real_time(
        &self,
        cfg: &AppConfig,
        repeat: u8,
        volume: u8,
        now: DateTime<Local>,
    ) {
        let tts_text = if cfg.chime_tts_enabled {
            Some(render_tts_template(
                &cfg.chime_tts_template,
                now.hour(),
                now.minute(),
            ))
        } else {
            None
        };
        self.play_chime(
            repeat,
            volume,
            cfg.chime_play_sound,
            None,
            tts_text,
            cfg.tts_engine,
            cfg.edge_tts_voice.clone(),
            cfg.tts_rate,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn play_meal(
        &self,
        repeat: u8,
        volume: u8,
        custom_path: Option<String>,
        chime_with_custom: bool,
        tts_text: Option<String>,
        tts_engine: TtsEngine,
        edge_voice: String,
        tts_rate: i8,
    ) {
        self.live_volume.store(volume.min(100), Ordering::Relaxed);
        let gen_id = self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.audio_tx.send(AudioCommand::StopActive);
        let _ = self.seq_tx.send(SequenceJob {
            gen_id,
            kind: SequenceKind::Meal {
                repeat,
                custom_path,
                chime_with_custom,
                tts_text,
                tts_engine,
                edge_voice,
                tts_rate,
            },
        });
    }

    pub fn set_live_volume(&self, volume: u8) {
        let clamped = volume.min(100);
        self.live_volume.store(clamped, Ordering::Relaxed);
        let _ = self.audio_tx.send(AudioCommand::SetLiveVolume(clamped));
    }

    /// 立即平滑停止当前正在播放的报时/语音（例如托盘切换为“静音模式”时调用）
    pub fn stop_active(&self) {
        self.gen_counter.fetch_add(1, Ordering::SeqCst);
        let _ = self.audio_tx.send(AudioCommand::StopActive);
    }

    /// 清理已完成播放的空闲解码器与 Edge TTS 内存缓存，配合后台内存回收降低驻留占用
    pub fn trim_idle(&self) {
        if let Ok(mut cache) = self.tts_cache.lock() {
            cache.clear();
        }
        let _ = self.audio_tx.send(AudioCommand::TrimIdle);
    }

    pub fn shutdown(&self) {
        self.gen_counter.fetch_add(1, Ordering::SeqCst);
        let _ = self.audio_tx.send(AudioCommand::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_sec_ms_gec_format() {
        let token = generate_sec_ms_gec(1_731_570_000);
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| matches!(c, '0'..='9' | 'A'..='F')));
        // 同一个 5 分钟时间窗口内应保持幂等一致
        assert_eq!(token, generate_sec_ms_gec(1_731_570_120));
    }

    #[test]
    fn test_build_edge_ssml_escapes_xml_and_formats_rate() {
        let ssml = build_edge_ssml("现在时间 & <12:00>", "zh-CN-YunxiNeural", 2);
        assert!(ssml.contains("name='zh-CN-YunxiNeural'"));
        assert!(ssml.contains("rate='+20%'"));
        assert!(ssml.contains("现在时间 &amp; &lt;12:00&gt;"));

        let slow_ssml = build_edge_ssml("休息一下", "", -3);
        assert!(slow_ssml.contains("name='zh-CN-XiaoxiaoNeural'"));
        assert!(slow_ssml.contains("rate='-30%'"));
    }

    #[test]
    fn test_extract_audio_chunk_from_binary_frame() {
        let header = b"X-RequestId:123\r\nContent-Type:audio/mpeg\r\nPath:audio\r\n";
        let len_be = (header.len() as u16).to_be_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(&len_be);
        frame.extend_from_slice(header);
        frame.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);

        let chunk = extract_audio_chunk_from_binary_frame(&frame).unwrap();
        assert_eq!(chunk, &[0xFF, 0xFB, 0x90, 0x00]);

        let non_audio_hdr = b"Path:turn.start\r\n";
        let mut frame2 = Vec::new();
        frame2.extend_from_slice(&(non_audio_hdr.len() as u16).to_be_bytes());
        frame2.extend_from_slice(non_audio_hdr);
        assert!(extract_audio_chunk_from_binary_frame(&frame2).is_none());
    }

    struct VecTestSource {
        samples: Vec<f32>,
        idx: usize,
        channels: u16,
        sample_rate: u32,
    }

    impl Iterator for VecTestSource {
        type Item = f32;

        fn next(&mut self) -> Option<Self::Item> {
            let val = self.samples.get(self.idx).copied();
            if val.is_some() {
                self.idx += 1;
            }
            val
        }
    }

    impl Source for VecTestSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn total_duration(&self) -> Option<Duration> {
            let frames = self.samples.len() as u64 / u64::from(self.channels);
            Some(Duration::from_millis((frames * 1000) / u64::from(self.sample_rate)))
        }
    }

    #[test]
    fn test_fade_envelope_source_normal_and_stereo_symmetry() {
        // 1000Hz 立体声 100 帧（200 采样），淡入 20ms（20帧/40采样），淡出 20ms（20帧/40采样）
        let src = VecTestSource {
            samples: vec![1.0; 200],
            idx: 0,
            channels: 2,
            sample_rate: 1000,
        };
        let faded: Vec<f32> = FadeEnvelopeSource::new(
            src,
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .collect();

        assert_eq!(faded.len(), 200);
        // 第 0 帧左右声道从 0.0 起步，且左右声道严格对称
        assert!((faded[0] - 0.0).abs() < 1e-6);
        assert!((faded[1] - faded[0]).abs() < 1e-6);
        // 淡入过程中单调递增
        assert!(faded[10] > faded[2] && faded[20] > faded[10]);
        // 中段保持 1.0 原增益
        for &s in &faded[60..120] {
            assert!((s - 1.0).abs() < 1e-6, "middle sample should stay 1.0, got {s}");
        }
        // 尾段单调递减并最终平滑归零，且左右声道对称
        assert!(faded[170] > faded[190]);
        assert!((faded[198] - 0.0).abs() < 1e-6);
        assert!((faded[199] - faded[198]).abs() < 1e-6);
    }

    #[test]
    fn test_fade_envelope_source_short_clip_and_empty() {
        // 短音频仅 20 帧（40采样），小于淡入+淡出窗口时应自动收敛不压暗中段峰值
        let short_src = VecTestSource {
            samples: vec![1.0; 40],
            idx: 0,
            channels: 2,
            sample_rate: 1000,
        };
        let faded: Vec<f32> = FadeEnvelopeSource::new(
            short_src,
            Duration::from_millis(180),
            Duration::from_millis(240),
        )
        .collect();
        assert_eq!(faded.len(), 40);
        assert!((faded[0] - 0.0).abs() < 1e-6);
        assert!(faded.iter().any(|&v| v > 0.9), "short clip should still reach peak");
        assert!((faded[39] - 0.0).abs() < 1e-6);

        let empty_src = VecTestSource {
            samples: vec![],
            idx: 0,
            channels: 2,
            sample_rate: 1000,
        };
        let empty_out: Vec<f32> = FadeEnvelopeSource::new(
            empty_src,
            Duration::from_millis(180),
            Duration::from_millis(240),
        )
        .collect();
        assert!(empty_out.is_empty());
    }

    #[test]
    fn test_compute_ducked_volume() {
        assert!((compute_ducked_volume(0.8).unwrap() - 0.2).abs() < 1e-5);
        assert!((compute_ducked_volume(0.12).unwrap() - AUDIO_DUCK_FLOOR_VOL).abs() < 1e-5);
        assert!(compute_ducked_volume(0.05).is_none());
        assert!(compute_ducked_volume(0.0).is_none());
        assert!(compute_ducked_volume(f32::NAN).is_none());
    }
}
