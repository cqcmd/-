use crate::config::{render_tts_template, AppConfig, TtsEngine, DEFAULT_EDGE_TTS_VOICE};
use crate::win_sys;
use chrono::{DateTime, Local, Timelike, Utc};
use rodio::{Decoder, OutputStream, Sink, Source};
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::fs::File;
use std::io::{BufReader, Cursor, ErrorKind};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::Message;

/// 编译期嵌入可执行文件内部的提示音资源（保证单文件零外置依赖）
static CHIME_WAV_BYTES: &[u8] = include_bytes!("../assets/chime.wav");
static MEAL_WAV_BYTES: &[u8] = include_bytes!("../assets/meal.wav");

const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const EDGE_SEC_MS_GEC_VERSION: &str = "2-143.0.3650.75";
const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";
const EDGE_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
const WIN_EPOCH_SECS: u64 = 11_644_473_600;
const EDGE_TTS_MAX_CACHE_ENTRIES: usize = 6;
const EDGE_TTS_MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;
const EDGE_TTS_MAX_AUDIO_BYTES: usize = 4 * 1024 * 1024;

static EDGE_CLOCK_SKEW_SECS: AtomicI64 = AtomicI64::new(0);

/// 提示音与自定义音频默认淡入/淡出时长（避免网吧公放或耳机环境下起止突兀）
const AUDIO_FADE_IN_MS: u64 = 180;
const AUDIO_FADE_OUT_MS: u64 = 240;
/// 语音流默认淡入/淡出时长（短包络去爆音且不吞字头字尾）
const SPEECH_FADE_IN_MS: u64 = 90;
const SPEECH_FADE_OUT_MS: u64 = 130;
/// 打断切换旧音频时的平滑渐隐时长
const AUDIO_SWITCH_FADE_MS: u64 = 80;

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
    active_seq_gen: Arc<AtomicU64>,
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

const EDGE_SHA256_INITIAL: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];
const EDGE_SHA256_STATE: [u32; 8] = [
    0xFD9F_BEA2,
    0x9124_3CE7,
    0xBE49_9094,
    0xCD2E_1E0A,
    0x6C00_6F49,
    0x04BD_35AB,
    0xFEB0_A7E4,
    0x03EF_FBD2,
];
const EDGE_SHA256_SCHEDULE: [u32; 14] = [
    0xEB24_BF9E,
    0x1558_691F,
    0xA0C0_9EE2,
    0x0CFA_32BA,
    0xA622_EE8C,
    0xD1DD_9491,
    0xC0DD_25A0,
    0xF9E7_0AB6,
    0xF0C3_1573,
    0x7058_CE4E,
    0x7188_E606,
    0x00E5_AB33,
    0xD258_B060,
    0x7276_6564,
];
const EDGE_SHA256_ROUND_CONSTANTS: [u32; 64] = [
    0x428A_2F98, 0x7137_4491, 0xB5C0_FBCF, 0xE9B5_DBA5,
    0x3956_C25B, 0x59F1_11F1, 0x923F_82A4, 0xAB1C_5ED5,
    0xD807_AA98, 0x1283_5B01, 0x2431_85BE, 0x550C_7DC3,
    0x72BE_5D74, 0x80DE_B1FE, 0x9BDC_06A7, 0xC19B_F174,
    0xE49B_69C1, 0xEFBE_4786, 0x0FC1_9DC6, 0x240C_A1CC,
    0x2DE9_2C6F, 0x4A74_84AA, 0x5CB0_A9DC, 0x76F9_88DA,
    0x983E_5152, 0xA831_C66D, 0xB003_27C8, 0xBF59_7FC7,
    0xC6E0_0BF3, 0xD5A7_9147, 0x06CA_6351, 0x1429_2967,
    0x27B7_0A85, 0x2E1B_2138, 0x4D2C_6DFC, 0x5338_0D13,
    0x650A_7354, 0x766A_0ABB, 0x81C2_C92E, 0x9272_2C85,
    0xA2BF_E8A1, 0xA81A_664B, 0xC24B_8B70, 0xC76C_51A3,
    0xD192_E819, 0xD699_0624, 0xF40E_3585, 0x106A_A070,
    0x19A4_C116, 0x1E37_6C08, 0x2748_774C, 0x34B0_BCB5,
    0x391C_0CB3, 0x4ED8_AA4A, 0x5B9C_CA4F, 0x682E_6FF3,
    0x748F_82EE, 0x78A5_636F, 0x84C8_7814, 0x8CC7_0208,
    0x90BE_FFFA, 0xA450_6CEB, 0xBEF9_A3F7, 0xC671_78F2,
];
const EDGE_SHA256_ADJUSTMENT: u32 = 0xC9DC_06A7;
const EDGE_SHA256_SIGMA0_ADJUSTMENT: u32 = 0x059C_0B80;
const EDGE_SHA256_SIGMA1_ADJUSTMENT: u32 = 0x000B_92C0;
const EDGE_CS64_COEFFICIENTS: [(u32, u32); 14] = [
    (0xE82E_91B1, 0xB327_00D0),
    (0x44DE_24C6, 0x88DF_F51D),
    (0x3BEB_65BC, 0x2951_4038),
    (0xC89C_5933, 0xB328_CBDF),
    (0x8D1E_C211, 0xD405_B4D5),
    (0x314E_F5D5, 0x26E5_5F3C),
    (0xE924_91B1, 0x6078_01C6),
    (0xF0A5_B9D5, 0x82B2_C1FB),
    (0x0606_4CF3, 0xFA51_A3DA),
    (0xDA79_FA60, 0x207C_C464),
    (0xBED2_0560, 0x3C3C_74F3),
    (0x89F9_C9F8, 0xD2D1_4B61),
    (0x35FA_38C6, 0xBB04_A2C1),
    (0xCB49_E3C6, 0xC562_6167),
];
const EDGE_CS64_FACTOR: u32 = 0x8289_3933;
const EDGE_CS64_ADJUSTMENT0: u32 = 0xF2F9_0B1A;
const EDGE_CS64_ADJUSTMENT1: u32 = 0x0B1A_0000;

#[inline]
fn edge_sigma0(v: u32) -> u32 {
    v.rotate_right(7) ^ v.rotate_right(18) ^ (v >> 3)
}

#[inline]
fn edge_sigma1(v: u32) -> u32 {
    v.rotate_right(17) ^ v.rotate_right(19) ^ (v >> 10)
}

fn edge_expand_sha256_schedule(words: &mut Vec<u32>, first_word: usize) {
    for idx in first_word..64 {
        let s1 = edge_sigma1(words[idx - 2]);
        let s0 = edge_sigma0(words[idx - 15]);
        let val = s1
            .wrapping_add(words[idx - 7])
            .wrapping_add(s0)
            .wrapping_add(words[idx - 16]);
        words.push(val);
    }
}

fn edge_sha256_rounds(state: [u32; 8], words: &[u32], first_round: usize) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for (idx, &word) in words.iter().enumerate().take(64).skip(first_round) {
        let k = if first_round == 14 && idx == 14 {
            EDGE_SHA256_ADJUSTMENT
        } else {
            EDGE_SHA256_ROUND_CONSTANTS[idx]
        };
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = g ^ (e & (f ^ g));
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k)
            .wrapping_add(word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) | (c & (a | b));
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    [a, b, c, d, e, f, g, h]
}

fn edge_sha256_init(data: &[u8]) -> [u32; 8] {
    let mut words = Vec::with_capacity(64);
    words.resize(14, 0u32);
    words.push((u32::from(data[0]) << 16) | (u32::from(data[1]) << 8) | u32::from(data[2]));
    words.push(u32::from_be_bytes([data[3], data[4], data[5], data[6]]));

    for idx in 16..30 {
        let mut val = EDGE_SHA256_SCHEDULE[idx - 16];
        if idx - 2 >= 14 {
            let s1 = edge_sigma1(words[idx - 2]);
            val = val.wrapping_add(if idx == 16 {
                s1 ^ EDGE_SHA256_SIGMA1_ADJUSTMENT
            } else {
                s1
            });
        }
        if idx - 7 >= 14 {
            val = val.wrapping_add(words[idx - 7]);
        }
        if idx - 15 >= 14 {
            let s0 = edge_sigma0(words[idx - 15]);
            val = val.wrapping_add(if idx == 29 {
                s0 ^ EDGE_SHA256_SIGMA0_ADJUSTMENT
            } else {
                s0
            });
        }
        words.push(val);
    }

    let w30 = edge_sigma1(words[28])
        .wrapping_add(words[23])
        .wrapping_add(edge_sigma0(words[15]))
        .wrapping_add(words[14])
        .wrapping_add(EDGE_SHA256_ADJUSTMENT)
        .wrapping_sub(EDGE_SHA256_ROUND_CONSTANTS[14]);
    words.push(w30);

    edge_expand_sha256_schedule(&mut words, 31);
    let working = edge_sha256_rounds(EDGE_SHA256_STATE, &words, 14);
    let mut out = [0u32; 8];
    for i in 0..8 {
        out[i] = EDGE_SHA256_INITIAL[i].wrapping_add(working[i]);
    }
    out
}

fn edge_sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut state = edge_sha256_init(&data[..7]);
    let length = 57usize + data.len();
    let pad_zeros = (55isize - (length as isize)).rem_euclid(64) as usize;

    let mut padded = Vec::with_capacity(data.len() - 7 + 1 + pad_zeros + 8);
    padded.extend_from_slice(&data[7..]);
    padded.push(0x80);
    padded.resize(padded.len() + pad_zeros, 0u8);
    padded.extend_from_slice(&((length as u64) * 8).to_be_bytes());

    for block in padded.chunks_exact(64) {
        let mut words = Vec::with_capacity(64);
        for chunk in block.chunks_exact(4) {
            words.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        edge_expand_sha256_schedule(&mut words, 16);
        let working = edge_sha256_rounds(state, &words, 0);
        for i in 0..8 {
            state[i] = state[i].wrapping_add(working[i]);
        }
    }

    let mut digest = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

fn edge_cs64_digest(data: &[u8], digest: &[u8; 32]) -> (u32, u32) {
    let key_a = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) | 1;
    let key_b = u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) | 1;
    let factors = [
        key_a.wrapping_mul(EDGE_CS64_FACTOR),
        key_b.wrapping_mul(EDGE_CS64_FACTOR),
    ];

    let mut terms = Vec::with_capacity(32);
    let mut offsets = Vec::with_capacity(32);
    for (idx, &(coeff, off)) in EDGE_CS64_COEFFICIENTS.iter().enumerate() {
        terms.push(coeff.wrapping_mul(factors[idx % 2]));
        offsets.push(off);
    }

    let mut prefixed = Vec::with_capacity(1 + data.len() + 4);
    prefixed.push(0);
    prefixed.extend_from_slice(data);
    while prefixed.len() % 4 != 0 {
        prefixed.push(0);
    }
    let mut data_words: Vec<u32> = prefixed
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if data_words.len() % 2 == 1 {
        data_words.push(0);
    }

    for (idx, word) in data_words.into_iter().enumerate() {
        let mut term = word.wrapping_mul(if idx % 2 == 0 { key_a } else { key_b });
        let mut offset = word
            .rotate_left(16)
            .wrapping_mul(if idx % 2 == 0 { 0xEF05_69FB } else { 0xC317_13DB });
        if idx == 0 {
            term = term.wrapping_add(EDGE_CS64_ADJUSTMENT0.wrapping_mul(factors[0]));
            offset = offset.wrapping_add(EDGE_CS64_ADJUSTMENT1);
        }
        terms.push(term);
        offsets.push(offset);
    }

    let mut swap_acc = 0u32;
    let mut swap_last = 0u32;
    let mut state = 0u32;
    let mut acc = 0u32;

    for idx in (0..terms.len()).step_by(2) {
        let (x, y) = (terms[idx], terms[idx + 1]);
        let t1 = offsets[idx].wrapping_add(x).rotate_left(16);
        let q1 = t1.wrapping_mul(0x76F5_0F34);
        swap_acc = swap_acc.wrapping_add(q1.rotate_left(16).wrapping_mul(0xC3EF_EA97).wrapping_add(q1));

        let t2 = offsets[idx + 1].wrapping_add(y).rotate_left(16);
        let q2 = t2.wrapping_mul(0x1881_CE3C);
        swap_last = q2.rotate_left(16).wrapping_mul(0x35BD_1EC9).wrapping_add(q2);
        swap_acc = swap_acc.wrapping_add(swap_last);

        state = state.wrapping_mul(key_a).wrapping_add(x);
        state = state.rotate_left(16).wrapping_mul(0xCF98_B111);
        state = state.rotate_left(16).wrapping_mul(0x8708_5B9F);
        state = state.rotate_left(16).wrapping_mul(0x12CE_B96D);
        state = state.rotate_left(16).wrapping_mul(0x257E_1D83);
        acc = acc.wrapping_add(state);

        let mut tb = state.wrapping_mul(key_b).wrapping_add(y);
        tb = tb.rotate_left(16).wrapping_mul(0xA274_16F5);
        tb = tb.rotate_left(16).wrapping_mul(0xD383_96FF);
        state = tb
            .rotate_left(16)
            .wrapping_mul(0x7C93_2B89)
            .rotate_left(16)
            .wrapping_mul(0xBFA4_9F69);
        acc = acc.wrapping_add(state);
    }

    (swap_last ^ state, swap_acc ^ acc)
}

/// 计算 Microsoft Edge Read Aloud WebSocket 接口所需的 v2 Sec-MS-GEC 校验令牌
pub fn generate_sec_ms_gec(unix_secs: u64) -> String {
    let win_secs = unix_secs.saturating_add(WIN_EPOCH_SECS);
    let rounded_secs = win_secs - (win_secs % 300);
    let ticks = u128::from(rounded_secs) * 10_000_000;
    let payload = format!("{ticks}{EDGE_TRUSTED_CLIENT_TOKEN}");
    let digest = edge_sha256_digest(payload.as_bytes());
    let (cs_lo, cs_hi) = edge_cs64_digest(payload.as_bytes(), &digest);

    let mut hex = String::with_capacity(81);
    use std::fmt::Write;
    for byte in cs_lo.to_le_bytes() {
        let _ = write!(&mut hex, "{byte:02X}");
    }
    for byte in cs_hi.to_le_bytes() {
        let _ = write!(&mut hex, "{byte:02X}");
    }
    hex.push('-');
    for byte in digest {
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

fn current_edge_unix_secs() -> u64 {
    let raw = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let skew = EDGE_CLOCK_SKEW_SECS.load(Ordering::Relaxed);
    raw.saturating_add(skew).max(0) as u64
}

fn synthesize_edge_tts_mp3_once(
    trimmed: &str,
    voice: &str,
    rate: i8,
    gen_id: u64,
    gen_seq: &AtomicU64,
    allow_clock_skew_retry: bool,
) -> Result<Vec<u8>, String> {
    if gen_seq.load(Ordering::SeqCst) != gen_id {
        return Err("cancelled".to_string());
    }

    let conn_id = generate_request_id_hex();
    let muid = generate_request_id_hex().to_ascii_uppercase();
    let sec_ms_gec = generate_sec_ms_gec(current_edge_unix_secs());
    let url = format!(
        "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken={EDGE_TRUSTED_CLIENT_TOKEN}&ConnectionId={conn_id}&Sec-MS-GEC={sec_ms_gec}&Sec-MS-GEC-Version={EDGE_SEC_MS_GEC_VERSION}"
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
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        if let Ok(cookie_val) = HeaderValue::from_str(&format!("muid={muid};")) {
            headers.insert("Cookie", cookie_val);
        }
    }

    let addrs = ("speech.platform.bing.com", 443)
        .to_socket_addrs()
        .map_err(|e| format!("DNS 解析 speech.platform.bing.com 失败：{e}"))?;

    let mut tcp_stream: Option<TcpStream> = None;
    let mut last_err = String::from("无可用网络地址");
    for addr in addrs {
        if gen_seq.load(Ordering::SeqCst) != gen_id {
            return Err("cancelled".to_string());
        }
        match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
            Ok(s) => {
                tcp_stream = Some(s);
                break;
            }
            Err(err) => {
                last_err = err.to_string();
            }
        }
    }
    let stream =
        tcp_stream.ok_or_else(|| format!("连接 Edge TTS 服务器超时或被拦截：{last_err}"))?;
    // 采用 120ms 短读取超时以便切换音色时即时取消，同时限制总等待不超过 8 秒
    let _ = stream.set_read_timeout(Some(Duration::from_millis(120)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(4)));

    let (mut socket, _resp) = match tungstenite::client_tls(request, stream) {
        Ok(pair) => pair,
        Err(tungstenite::Error::Http(resp)) if allow_clock_skew_retry => {
            if let Some(date_hdr) = resp.headers().get("Date").and_then(|v| v.to_str().ok()) {
                if let Ok(server_dt) = DateTime::parse_from_rfc2822(date_hdr) {
                    let local_raw = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let skew = server_dt.timestamp().saturating_sub(local_raw);
                    EDGE_CLOCK_SKEW_SECS.store(skew, Ordering::Relaxed);
                    return synthesize_edge_tts_mp3_once(
                        trimmed, voice, rate, gen_id, gen_seq, false,
                    );
                }
            }
            return Err(format!("Edge TTS HTTP 拒绝连接：{}", resp.status()));
        }
        Err(err) => {
            return Err(format!("Edge TTS WebSocket TLS 握手失败：{err}"));
        }
    };

    let timestamp = Utc::now()
        .format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string();

    let speech_config_msg = format!(
        "X-Timestamp:{timestamp}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"true\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\"}}}}}}}}\r\n"
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
    let started = Instant::now();
    loop {
        if gen_seq.load(Ordering::SeqCst) != gen_id {
            let _ = socket.close(None);
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(8) {
            let _ = socket.close(None);
            if audio_bytes.is_empty() {
                return Err("接收 Edge TTS 音频流超时".to_string());
            }
            break;
        }
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
            Err(tungstenite::Error::Io(ref io_err))
                if matches!(io_err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue;
            }
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

/// 通过 Microsoft Edge TTS 在线 WebSocket 接口合成 MP3 字节流（带时钟偏差自修正与即时取消保护）
fn synthesize_edge_tts_mp3(
    text: &str,
    voice: &str,
    rate: i8,
    gen_id: u64,
    gen_seq: &AtomicU64,
) -> Result<Vec<u8>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("empty tts text".to_string());
    }
    synthesize_edge_tts_mp3_once(trimmed, voice, rate, gen_id, gen_seq, true)
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
        &format!(
            "speaking via {:?} voice={edge_voice} rate={tts_rate} vol={volume}: \"{text}\"",
            tts_engine
        ),
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
            None => synthesize_edge_tts_mp3(text, voice, tts_rate, gen_id, gen_seq).map(|bytes| {
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
                if err == "cancelled" || gen_seq.load(Ordering::SeqCst) != gen_id {
                    return false;
                }
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
    match win_sys::speak_text_sapi_cancellable(text, volume, tts_rate, || {
        gen_seq.load(Ordering::SeqCst) != gen_id
    }) {
        Ok(completed) => completed && gen_seq.load(Ordering::SeqCst) == gen_id,
        Err(err) => {
            win_sys::log_warn("tts", &err);
            gen_seq.load(Ordering::SeqCst) == gen_id
        }
    }
}

impl AudioEngine {
    pub fn start() -> Self {
        let (audio_tx, audio_rx) = mpsc::channel::<AudioCommand>();
        let (seq_tx, seq_rx) = mpsc::channel::<SequenceJob>();
        let gen_counter = Arc::new(AtomicU64::new(0));
        let active_seq_gen = Arc::new(AtomicU64::new(0));
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
        let active_seq_clone = Arc::clone(&active_seq_gen);
        let vol_seq = Arc::clone(&live_volume);
        let cache_seq = Arc::clone(&tts_cache);
        thread::Builder::new()
            .name("audio-sequence-worker".to_string())
            .spawn(move || {
                while let Ok(job) = seq_rx.recv() {
                    if gen_seq.load(Ordering::SeqCst) != job.gen_id {
                        let _ = active_seq_clone.compare_exchange(
                            job.gen_id,
                            0,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
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

                    let _ = active_seq_clone.compare_exchange(
                        job.gen_id,
                        0,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                }
            })
            .expect("Failed to spawn audio sequence worker thread");

        Self {
            audio_tx,
            seq_tx,
            gen_counter,
            active_seq_gen,
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
    ) -> u64 {
        self.live_volume.store(volume.min(100), Ordering::Relaxed);
        let gen_id = self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
        self.active_seq_gen.store(gen_id, Ordering::SeqCst);
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
        gen_id
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
        let _ = self.play_chime(
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
        self.active_seq_gen.store(gen_id, Ordering::SeqCst);
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

    pub fn is_playing_gen(&self, gen_id: u64) -> bool {
        gen_id != 0 && self.active_seq_gen.load(Ordering::SeqCst) == gen_id
    }

    /// 立即平滑停止当前正在播放的报时/语音（例如托盘切换为“静音模式”或切换音色时调用）
    pub fn stop_active(&self) {
        self.gen_counter.fetch_add(1, Ordering::SeqCst);
        self.active_seq_gen.store(0, Ordering::SeqCst);
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
        let token_a = generate_sec_ms_gec(1_731_570_000);
        assert_eq!(token_a.len(), 81);
        assert_eq!(
            token_a,
            "8DE5A7CF1DA2BF9C-6347C725761C960EAE4066BF45CFF934648415B93EA251E031565E06FCC93160"
        );
        // 同一个 5 分钟时间窗口内应保持幂等一致
        assert_eq!(token_a, generate_sec_ms_gec(1_731_570_120));
        assert_eq!(
            generate_sec_ms_gec(1_700_000_000),
            "2CD23A2CA0B2595A-F3C846C370E05F9CC83ACC5F995C5F5D87A4D0F14CE8936A0F902E64099E38A2"
        );
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
