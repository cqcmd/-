use crate::config::CustomAlarm;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const DEFAULT_WEB_CONSOLE_PORT: u16 = 3000;
pub const DEFAULT_WEB_CONSOLE_URL: &str = "http://127.0.0.1:3000";

#[derive(Debug, Clone, Default, Serialize)]
pub struct WebSnapshot {
    pub version: String,
    pub current_time: String,
    pub current_date: String,
    pub mute_mode: bool,
    pub is_quiet_now: bool,
    pub is_audio_playing: bool,
    pub quiet_summary: String,
    pub next_event_title: String,
    pub next_event_time: String,
    pub next_event_countdown: String,
    pub chime_minutes: String,
    pub enabled_alarms_count: usize,
    pub tts_engine: String,
    pub tts_engine_index: i32,
    pub edge_tts_voice: String,
    pub edge_voice_index: i32,
    pub tts_rate: i8,
    pub tts_preview_status: String,
    pub volume: u8,
    pub theme_index: i32,
    pub ntp_status: String,
    pub is_syncing: bool,
    pub memory_ws_mb: f64,
    pub memory_private_mb: f64,
    pub process_cpu_pct: f64,
    pub system_cpu_pct: f64,
    pub uptime_text: String,
    pub config_path: String,
    pub log_path: String,
    pub today_calendar_preview: String,
    pub update_checking: bool,
    pub has_update: bool,
    pub update_version: String,
    pub update_status_text: String,
    pub update_notes: String,
    pub update_downloading: bool,
    pub update_progress: i32,
    pub update_download_status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WebConfigView {
    pub hourly_chime_enabled: bool,
    pub half_hour_chime_enabled: bool,
    pub custom_chime_minutes_str: String,
    pub chime_repeat_count: u8,
    pub chime_play_sound: bool,
    pub chime_tts_enabled: bool,
    pub chime_tts_template: String,
    pub holiday_enabled: bool,
    pub holiday_greeting_template: String,
    pub holiday_custom_audio_path: String,
    pub quiet_enabled: bool,
    pub quiet_start_hour: u8,
    pub quiet_start_minute: u8,
    pub quiet_end_hour: u8,
    pub quiet_end_minute: u8,
    pub alarms: Vec<CustomAlarm>,
    pub tts_engine_index: i32,
    pub edge_voice_index: i32,
    pub tts_rate: i8,
    pub volume: u8,
    pub ntp_server: String,
    pub auto_start: bool,
    pub theme_index: i32,
    pub update_check_url: String,
    pub update_check_interval_hours: u16,
    pub has_github_token: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WebConfigPatch {
    pub hourly_chime_enabled: Option<bool>,
    pub half_hour_chime_enabled: Option<bool>,
    pub custom_chime_minutes_str: Option<String>,
    pub chime_repeat_count: Option<u8>,
    pub chime_play_sound: Option<bool>,
    pub chime_tts_enabled: Option<bool>,
    pub chime_tts_template: Option<String>,
    pub holiday_enabled: Option<bool>,
    pub holiday_greeting_template: Option<String>,
    pub holiday_custom_audio_path: Option<String>,
    pub quiet_enabled: Option<bool>,
    pub quiet_start_hour: Option<u8>,
    pub quiet_start_minute: Option<u8>,
    pub quiet_end_hour: Option<u8>,
    pub quiet_end_minute: Option<u8>,
    pub alarms: Option<Vec<CustomAlarm>>,
    pub tts_engine_index: Option<i32>,
    pub edge_voice_index: Option<i32>,
    pub tts_rate: Option<i8>,
    pub volume: Option<u8>,
    pub ntp_server: Option<String>,
    pub auto_start: Option<bool>,
    pub theme_index: Option<i32>,
    pub update_check_url: Option<String>,
    pub update_check_interval_hours: Option<u16>,
    pub update_github_token: Option<String>,
    pub clear_github_token: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct VoicePreviewPayload {
    tts_engine_index: Option<i32>,
    edge_voice_index: Option<i32>,
    tts_rate: Option<i8>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct TestAlarmPayload {
    alarm_id: Option<u64>,
    alarm: Option<CustomAlarm>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct VolumePayload {
    volume: u8,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ThemePayload {
    theme_index: i32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct NtpPayload {
    server: Option<String>,
}

#[derive(Debug, Clone)]
pub enum WebAction {
    TestChime,
    StopAudio,
    TestVoice {
        tts_engine_index: Option<i32>,
        edge_voice_index: Option<i32>,
        tts_rate: Option<i8>,
    },
    TestAlarm {
        alarm_id: Option<u64>,
        inline_alarm: Option<CustomAlarm>,
    },
    ToggleMute,
    SyncNtp {
        server: Option<String>,
    },
    SetVolume(u8),
    SetTheme(i32),
    TrimMemory(Sender<Result<String, String>>),
    CheckUpdate,
    ApplyUpdate,
    OpenLogs,
    SaveConfig(WebConfigPatch, Sender<Result<String, String>>),
}

pub struct WebConsoleController {
    running: Arc<AtomicBool>,
    snapshot: Arc<Mutex<WebSnapshot>>,
    config_view: Arc<Mutex<WebConfigView>>,
    action_tx: Sender<WebAction>,
    active_url: String,
    handle: Option<JoinHandle<()>>,
}

impl WebConsoleController {
    pub fn new(action_tx: Sender<WebAction>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(Mutex::new(WebSnapshot::default())),
            config_view: Arc::new(Mutex::new(WebConfigView::default())),
            action_tx,
            active_url: DEFAULT_WEB_CONSOLE_URL.to_string(),
            handle: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn url(&self) -> &str {
        &self.active_url
    }

    pub fn update_snapshot(&self, snap: WebSnapshot) {
        if let Ok(mut guard) = self.snapshot.lock() {
            *guard = snap;
        }
    }

    pub fn update_config_view(&self, cfg_view: WebConfigView) {
        if let Ok(mut guard) = self.config_view.lock() {
            *guard = cfg_view;
        }
    }

    pub fn start(&mut self, port: u16) -> Result<String, String> {
        if self.is_running() {
            return Ok(self.active_url.clone());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }

        let bind_addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&bind_addr)
            .map_err(|e| format!("无法监听 {bind_addr}（端口可能已被其他程序占用）：{e}"))?;
        let _ = listener.set_nonblocking(true);

        let url = format!("http://127.0.0.1:{port}");
        self.active_url = url.clone();
        self.running.store(true, Ordering::SeqCst);

        let running_flag = Arc::clone(&self.running);
        let snap_ref = Arc::clone(&self.snapshot);
        let cfg_ref = Arc::clone(&self.config_view);
        let action_tx = self.action_tx.clone();

        let active_conns = Arc::new(AtomicUsize::new(0));
        let join_handle = thread::Builder::new()
            .name("web-console-http".to_string())
            .spawn(move || {
                while running_flag.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                            let snap_c = Arc::clone(&snap_ref);
                            let cfg_c = Arc::clone(&cfg_ref);
                            let tx_c = action_tx.clone();
                            if active_conns.load(Ordering::Relaxed) < 16 {
                                let conn_counter = Arc::clone(&active_conns);
                                conn_counter.fetch_add(1, Ordering::Relaxed);
                                let _ = thread::Builder::new()
                                    .name("web-console-req".to_string())
                                    .spawn(move || {
                                        handle_http_client(stream, &snap_c, &cfg_c, &tx_c);
                                        conn_counter.fetch_sub(1, Ordering::Relaxed);
                                    });
                            } else {
                                handle_http_client(stream, &snap_c, &cfg_c, &tx_c);
                            }
                        }
                        Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(40));
                        }
                        Err(_) => {
                            thread::sleep(Duration::from_millis(120));
                        }
                    }
                }
            })
            .map_err(|e| format!("启动本地 Web 控制台线程失败：{e}"))?;

        self.handle = Some(join_handle);
        Ok(url)
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WebConsoleController {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ParsedHttpRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers_str: &str) -> usize {
    for line in headers_str.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(len) = v.trim().parse::<usize>() {
                    return len.min(256 * 1024);
                }
            }
        }
    }
    0
}

fn read_http_request(stream: &mut TcpStream) -> Option<ParsedHttpRequest> {
    let mut raw = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut header_end: Option<usize> = None;

    while raw.len() < 16 * 1024 {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        raw.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&raw) {
            header_end = Some(pos);
            break;
        }
    }

    let hdr_end = header_end?;
    let headers_str = String::from_utf8_lossy(&raw[..hdr_end]).to_string();
    let content_len = parse_content_length(&headers_str);
    let body_start = hdr_end + 4;

    while raw.len() < body_start + content_len {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        raw.extend_from_slice(&chunk[..n]);
    }

    let first_line = headers_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let raw_target = parts.next().unwrap_or("/");
    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_target.to_string(), String::new()),
    };

    let actual_end = (body_start + content_len).min(raw.len());
    let body = if actual_end > body_start {
        raw[body_start..actual_end].to_vec()
    } else {
        Vec::new()
    };

    Some(ParsedHttpRequest {
        method,
        path,
        query,
        body,
    })
}

fn read_log_tail(path_str: &str, level_filter: &str, keyword: &str) -> serde_json::Value {
    let trimmed_path = path_str.trim();
    if trimmed_path.is_empty() {
        return serde_json::json!({
            "path": "",
            "lines": ["（日志路径尚未初始化）"]
        });
    }

    let Ok(mut file) = File::open(trimmed_path) else {
        return serde_json::json!({
            "path": trimmed_path,
            "lines": ["（暂未找到今日日志文件或暂无日志写入）"]
        });
    };

    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let read_start = file_len.saturating_sub(96 * 1024);
    let _ = file.seek(SeekFrom::Start(read_start));
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect();

    if read_start > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let level_up = level_filter.trim().to_ascii_uppercase();
    let kw_low = keyword.trim().to_ascii_lowercase();

    let filtered: Vec<String> = lines
        .into_iter()
        .filter(|line| {
            if !level_up.is_empty() && level_up != "ALL" {
                let tag = format!("[{level_up}");
                if !line.contains(&tag) {
                    return false;
                }
            }
            if !kw_low.is_empty() && !line.to_ascii_lowercase().contains(&kw_low) {
                return false;
            }
            true
        })
        .rev()
        .take(200)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|s| s.to_string())
        .collect();

    serde_json::json!({
        "path": trimmed_path,
        "lines": filtered
    })
}

fn extract_query_param<'a>(query: &'a str, key: &str) -> String {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return url_decode_lossy(v);
            }
        }
    }
    String::new()
}

fn url_decode_lossy(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (
                    (bytes[i + 1] as char).to_digit(16),
                    (bytes[i + 2] as char).to_digit(16),
                ) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn json_ok(msg: &str) -> Vec<u8> {
    serde_json::json!({ "ok": true, "message": msg })
        .to_string()
        .into_bytes()
}

fn json_err(msg: &str) -> Vec<u8> {
    serde_json::json!({ "ok": false, "error": msg })
        .to_string()
        .into_bytes()
}

fn send_sync_action<F>(
    stream: &mut TcpStream,
    action_tx: &Sender<WebAction>,
    build_action: F,
) where
    F: FnOnce(Sender<Result<String, String>>) -> WebAction,
{
    let (reply_tx, reply_rx) = mpsc::channel::<Result<String, String>>();
    if action_tx.send(build_action(reply_tx)).is_err() {
        let body = json_err("主程序消息通道已关闭");
        write_http_response(stream, "500 Internal Server Error", "application/json; charset=utf-8", &body);
        return;
    }

    match reply_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(msg)) => {
            let body = json_ok(&msg);
            write_http_response(stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        Ok(Err(err)) => {
            let body = json_err(&err);
            write_http_response(stream, "400 Bad Request", "application/json; charset=utf-8", &body);
        }
        Err(_) => {
            let body = json_err("操作等待主程序响应超时，请稍后重试");
            write_http_response(stream, "504 Gateway Timeout", "application/json; charset=utf-8", &body);
        }
    }
}

fn handle_http_client(
    mut stream: TcpStream,
    snap_ref: &Arc<Mutex<WebSnapshot>>,
    cfg_ref: &Arc<Mutex<WebConfigView>>,
    action_tx: &Sender<WebAction>,
) {
    let Some(req) = read_http_request(&mut stream) else {
        return;
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_http_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                WEB_CONSOLE_HTML.as_bytes(),
            );
        }
        ("GET", "/api/status") => {
            let json = snap_ref
                .lock()
                .ok()
                .and_then(|s| serde_json::to_string(&*s).ok())
                .unwrap_or_else(|| "{}".to_string());
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                json.as_bytes(),
            );
        }
        ("GET", "/api/config") => {
            let json = cfg_ref
                .lock()
                .ok()
                .and_then(|c| serde_json::to_string(&*c).ok())
                .unwrap_or_else(|| "{}".to_string());
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                json.as_bytes(),
            );
        }
        ("POST", "/api/config") => {
            match serde_json::from_slice::<WebConfigPatch>(&req.body) {
                Ok(patch) => {
                    send_sync_action(&mut stream, action_tx, |tx| WebAction::SaveConfig(patch, tx));
                }
                Err(err) => {
                    let body = json_err(&format!("配置参数 JSON 格式有误：{err}"));
                    write_http_response(&mut stream, "400 Bad Request", "application/json; charset=utf-8", &body);
                }
            }
        }
        ("GET", "/api/logs") => {
            let log_path = snap_ref
                .lock()
                .map(|s| s.log_path.clone())
                .unwrap_or_default();
            let level = extract_query_param(&req.query, "level");
            let kw = extract_query_param(&req.query, "q");
            let payload = read_log_tail(&log_path, &level, &kw).to_string();
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                payload.as_bytes(),
            );
        }
        ("POST", "/api/action/test-chime") => {
            let _ = action_tx.send(WebAction::TestChime);
            let body = json_ok("已触发测试报时");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/stop-audio") => {
            let _ = action_tx.send(WebAction::StopAudio);
            let body = json_ok("已停止当前音频或语音播报");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/test-voice") => {
            let payload = serde_json::from_slice::<VoicePreviewPayload>(&req.body).unwrap_or_default();
            let _ = action_tx.send(WebAction::TestVoice {
                tts_engine_index: payload.tts_engine_index,
                edge_voice_index: payload.edge_voice_index,
                tts_rate: payload.tts_rate,
            });
            let body = json_ok("已触发音色试听");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/test-alarm") => {
            let payload = serde_json::from_slice::<TestAlarmPayload>(&req.body).unwrap_or_default();
            let _ = action_tx.send(WebAction::TestAlarm {
                alarm_id: payload.alarm_id,
                inline_alarm: payload.alarm,
            });
            let body = json_ok("已触发自定义闹钟试听");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/toggle-mute") => {
            let _ = action_tx.send(WebAction::ToggleMute);
            let body = json_ok("已切换全局静音状态");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/sync-ntp") => {
            let payload = serde_json::from_slice::<NtpPayload>(&req.body).unwrap_or_default();
            let _ = action_tx.send(WebAction::SyncNtp {
                server: payload.server,
            });
            let body = json_ok("已发起网络校时");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/set-volume") => {
            if let Ok(payload) = serde_json::from_slice::<VolumePayload>(&req.body) {
                let vol = payload.volume.min(100);
                let _ = action_tx.send(WebAction::SetVolume(vol));
                let body = json_ok(&format!("已实时调节音量至 {vol}%"));
                write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
            } else {
                let body = json_err("音量参数无效");
                write_http_response(&mut stream, "400 Bad Request", "application/json; charset=utf-8", &body);
            }
        }
        ("POST", "/api/action/set-theme") => {
            if let Ok(payload) = serde_json::from_slice::<ThemePayload>(&req.body) {
                let _ = action_tx.send(WebAction::SetTheme(payload.theme_index));
                let body = json_ok("已切换界面主题");
                write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
            } else {
                let body = json_err("主题参数无效");
                write_http_response(&mut stream, "400 Bad Request", "application/json; charset=utf-8", &body);
            }
        }
        ("POST", "/api/action/trim-memory") => {
            send_sync_action(&mut stream, action_tx, WebAction::TrimMemory);
        }
        ("POST", "/api/action/check-update") => {
            let _ = action_tx.send(WebAction::CheckUpdate);
            let body = json_ok("已发起最新版本检查");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/apply-update") => {
            let _ = action_tx.send(WebAction::ApplyUpdate);
            let body = json_ok("已启动自动更新下载流程");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        ("POST", "/api/action/open-logs") => {
            let _ = action_tx.send(WebAction::OpenLogs);
            let body = json_ok("已在本机打开日志文件夹");
            write_http_response(&mut stream, "200 OK", "application/json; charset=utf-8", &body);
        }
        _ => {
            write_http_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Not Found",
            );
        }
    }
}

fn write_http_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

const WEB_CONSOLE_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>半整点提醒 · 本地管理控制台</title>
<style>
  :root{
    --bg:#F5F1E8;--card:#FCFBF7;--surface2:#F3EDE0;--border:#E2DAC6;
    --text:#292218;--sub:#574B3A;--muted:#7C6E5A;--accent:#B45309;--accent-fg:#FFFFFF;
    --ok:#1F7A4C;--warn:#B45309;--err:#B91C1C;--code:#EFE7D6;
  }
  body[data-theme="0"]{
    --bg:#0D1117;--card:#151B24;--surface2:#1C2430;--border:#303B4C;
    --text:#F8FAFC;--sub:#E2E8F0;--muted:#94A3B8;--accent:#38BDF8;--accent-fg:#082F49;
    --ok:#34D399;--warn:#FBBF24;--err:#F87171;--code:#0B0F14;
  }
  body[data-theme="1"]{
    --bg:#F4F6F9;--card:#FFFFFF;--surface2:#F8FAFC;--border:#E2E8F0;
    --text:#0F172A;--sub:#334155;--muted:#64748B;--accent:#2563EB;--accent-fg:#FFFFFF;
    --ok:#15803D;--warn:#D97706;--err:#DC2626;--code:#F1F5F9;
  }
  *{box-sizing:border-box}
  body{margin:0;padding:20px 16px 40px;font-family:"Microsoft YaHei UI",system-ui,sans-serif;background:var(--bg);color:var(--text);transition:background .15s,color .15s}
  .wrap{max-width:980px;margin:0 auto;display:grid;gap:14px}
  .topbar{display:flex;flex-wrap:wrap;justify-content:space-between;align-items:center;gap:12px}
  .brand{display:flex;align-items:baseline;gap:10px}
  .brand h1{margin:0;font-size:20px;font-weight:700}
  .ver{font-family:Consolas,monospace;color:var(--accent);font-size:13px;font-weight:700}
  .nav{display:flex;flex-wrap:wrap;gap:6px;background:var(--surface2);padding:4px;border-radius:10px;border:1px solid var(--border)}
  .nav button{border:none;background:transparent;color:var(--muted);padding:7px 14px;border-radius:7px;font-size:13px;font-weight:600;cursor:pointer}
  .nav button.active{background:var(--card);color:var(--text);box-shadow:0 1px 2px rgba(0,0,0,.08)}
  .panel{display:none;gap:14px}
  .panel.active{display:grid}
  .grid2{display:grid;grid-template-columns:repeat(auto-fit,minmax(310px,1fr));gap:14px}
  .card{background:var(--card);border:1px solid var(--border);border-radius:10px;padding:16px 18px;display:grid;gap:12px}
  .card-title{font-size:14px;font-weight:700;color:var(--text);display:flex;justify-content:space-between;align-items:center;gap:8px}
  .clock{font-family:Consolas,monospace;font-size:38px;font-weight:700;letter-spacing:1px;line-height:1.1}
  .row{display:flex;justify-content:space-between;align-items:center;gap:12px;font-size:13px}
  .muted{color:var(--muted);font-size:12px}
  .mono{font-family:Consolas,monospace}
  .btn-row{display:flex;flex-wrap:wrap;gap:8px;align-items:center}
  button{border:1px solid var(--border);background:var(--card);color:var(--text);padding:8px 13px;border-radius:8px;font-size:13px;font-weight:600;cursor:pointer}
  button:hover{border-color:var(--accent)}
  button:disabled{opacity:.55;cursor:not-allowed}
  button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
  button.danger{color:var(--err)}
  button.chip{padding:5px 10px;font-size:12px;border-radius:6px;background:var(--surface2)}
  button.chip.on{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
  .field{display:grid;gap:5px}
  .field label{font-size:12px;color:var(--sub);font-weight:600;display:flex;justify-content:space-between;align-items:center}
  input[type="text"],input[type="password"],input[type="number"],input[type="time"],input[type="date"],select,textarea{
    width:100%;padding:8px 10px;border-radius:7px;border:1px solid var(--border);
    background:var(--surface2);color:var(--text);font-size:13px;font-family:inherit;
  }
  .switch{display:flex;align-items:center;gap:8px;font-size:13px;cursor:pointer;user-select:none}
  .switch input{width:16px;height:16px;accent-color:var(--accent);cursor:pointer}
  .kv-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(165px,1fr));gap:10px}
  .metric{background:var(--surface2);border:1px solid var(--border);border-radius:8px;padding:10px 12px}
  .metric .k{font-size:11px;color:var(--muted)}
  .metric .v{font-size:16px;font-weight:700;font-family:Consolas,monospace;margin-top:2px}
  .alarm-list{display:grid;gap:8px}
  .alarm-item{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:10px;background:var(--surface2);border:1px solid var(--border);border-radius:8px;padding:10px 12px}
  .alarm-item-left{display:flex;align-items:center;gap:12px;min-width:220px;flex:1}
  .alarm-time{font-family:Consolas,monospace;font-size:20px;font-weight:700}
  .alarm-editor{background:var(--surface2);border:1.5px solid var(--accent);border-radius:9px;padding:14px;display:grid;gap:10px}
  .wd-row{display:flex;flex-wrap:wrap;gap:6px}
  .wd-row label{display:flex;align-items:center;gap:4px;padding:4px 8px;border:1px solid var(--border);border-radius:6px;background:var(--card);font-size:12px;cursor:pointer}
  .toast-bar{position:sticky;bottom:12px;z-index:20;padding:10px 14px;border-radius:8px;border:1px solid var(--border);background:var(--card);font-size:13px;display:flex;justify-content:space-between;align-items:center;box-shadow:0 4px 16px rgba(0,0,0,.1)}
  .log-box{background:var(--code);border:1px solid var(--border);border-radius:8px;padding:10px;height:360px;overflow:auto;font-family:Consolas,monospace;font-size:12px;line-height:1.5;white-space:pre-wrap;word-break:break-all}
  .log-warn{color:var(--warn)} .log-err{color:var(--err);font-weight:700} .log-info{color:var(--sub)} .log-dbg{color:var(--muted)}
  hr{border:none;border-top:1px solid var(--border);margin:2px 0}
</style>
</head>
<body data-theme="2">
<div class="wrap">
  <div class="topbar">
    <div class="brand">
      <h1>半整点提醒 · 本地管理控制台</h1>
      <span class="ver" id="ver">--</span>
    </div>
    <div class="nav">
      <button class="active" onclick="switchTab('overview',this)">总览与控制</button>
      <button onclick="switchTab('settings',this)">报时与自定义闹钟</button>
      <button onclick="switchTab('updates',this)">版本与更新</button>
      <button onclick="switchTab('logs',this)">运行日志</button>
    </div>
  </div>

  <!-- Tab 1: 总览与快捷控制 -->
  <div id="tab-overview" class="panel active">
    <div class="card">
      <div class="row">
        <div>
          <div class="clock" id="clock">--:--:--</div>
          <div class="muted" id="date" style="margin-top:4px">--</div>
        </div>
        <div style="text-align:right">
          <div id="stateBadge" style="font-weight:700;font-size:15px">--</div>
          <div class="muted" id="quietText">--</div>
          <div class="muted" id="calendarText" style="margin-top:2px">--</div>
        </div>
      </div>
      <hr>
      <div class="row">
        <span>下一提醒事件：<strong id="nextTitle">--</strong>（<span class="mono" id="nextTime">--</span>）</span>
        <span class="mono" style="font-weight:700;font-size:15px;color:var(--accent)" id="countdown">--</span>
      </div>
    </div>

    <div class="grid2">
      <div class="card">
        <div class="card-title">快捷控制与实时播报</div>
        <div class="btn-row">
          <button class="primary" onclick="postAction('/api/action/test-chime')">立即测试报时</button>
          <button onclick="postAction('/api/action/stop-audio')" id="stopAudioBtn">停止当前播报</button>
          <button onclick="postAction('/api/action/test-voice',{})">试听当前音色</button>
          <button onclick="postAction('/api/action/toggle-mute')" id="muteBtn">切换静音</button>
          <button onclick="postAction('/api/action/sync-ntp',{})" id="ntpBtn">立即 NTP 校时</button>
        </div>
        <div class="field">
          <label><span>全局音量</span><span class="mono" id="volLabel">80%</span></label>
          <input type="range" id="liveVol" min="0" max="100" value="80" oninput="onVolumeSlide(this.value)">
        </div>
        <div class="field">
          <label><span>界面主题</span></label>
          <div class="btn-row" id="themeBtns">
            <button class="chip" data-theme-idx="2" onclick="setTheme(2)">暖金咖啡馆</button>
            <button class="chip" data-theme-idx="0" onclick="setTheme(0)">暗夜曜石</button>
            <button class="chip" data-theme-idx="1" onclick="setTheme(1)">晨曦云母</button>
          </div>
        </div>
        <div class="muted" id="ttsStatusLine">语音状态：--</div>
      </div>

      <div class="card">
        <div class="card-title">
          <span>运行状态与资源占用</span>
          <button class="chip" onclick="postAction('/api/action/trim-memory')">释放内存</button>
        </div>
        <div class="kv-grid">
          <div class="metric"><div class="k">工作集内存</div><div class="v" id="mWs">-- MB</div></div>
          <div class="metric"><div class="k">私有内存</div><div class="v" id="mPriv">-- MB</div></div>
          <div class="metric"><div class="k">本进程 / 整机 CPU</div><div class="v" id="mCpu">-- / --</div></div>
          <div class="metric"><div class="k">运行时长</div><div class="v" id="mUp">--</div></div>
        </div>
        <div class="row"><span class="muted">报时分钟 / 已启用闹钟</span><span id="chimeAlarmStat">--</span></div>
        <div class="row"><span class="muted">当前语音</span><span id="curVoice">--</span></div>
        <div class="row"><span class="muted">校时状态</span><span id="ntpStatus">--</span></div>
        <div class="row"><span class="muted">配置文件</span><span class="mono muted" id="cfgPath" style="text-align:right;word-break:break-all">--</span></div>
      </div>
    </div>
  </div>

  <!-- Tab 2: 报时、节假日与自定义闹钟配置 -->
  <div id="tab-settings" class="panel">
    <div class="grid2">
      <div class="card">
        <div class="card-title">定时报时</div>
        <label class="switch"><input type="checkbox" id="cfgHourlyEnabled"> 启用整点报时 (:00)</label>
        <label class="switch"><input type="checkbox" id="cfgHalfHourEnabled"> 启用半点报时 (:30)</label>
        <div class="field">
          <label><span>额外报时分钟（可选）</span></label>
          <input type="text" id="cfgExtraMinutes" placeholder="如 15, 45">
        </div>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>重复次数 (1–10)</span></label><input type="number" id="cfgChimeRepeat" min="1" max="10"></div>
          <div class="field"><label><span>语音语速 (-5 ~ +5)</span></label><input type="number" id="cfgTtsRate" min="-5" max="5"></div>
        </div>
        <label class="switch"><input type="checkbox" id="cfgPlaySound"> 播放内置提示音</label>
        <label class="switch"><input type="checkbox" id="cfgChimeTts"> 文字转语音报时</label>
        <div class="field">
          <label><span>报时语音模板（{TIME} {TIME12} {PERIOD} {HH}:{MM}）</span></label>
          <input type="text" id="cfgChimeTpl" placeholder="现在时间，{TIME}">
        </div>
        <hr>
        <div class="card-title">节假日播报</div>
        <label class="switch"><input type="checkbox" id="cfgHolidayEnabled"> 启用节假日自动播报</label>
        <div class="field"><label><span>问候模板（{HOLIDAY} {GREETING} {LUNAR} {TIME}）</span></label><input type="text" id="cfgHolidayTpl"></div>
        <div class="field"><label><span>节庆音频路径（可选）</span></label><input type="text" id="cfgHolidayAudio" placeholder="留空使用内置提示音"></div>
      </div>

      <div class="card">
        <div class="card-title">语音引擎与系统设置</div>
        <div class="field">
          <label><span>语音合成引擎</span></label>
          <select id="cfgTtsEngine">
            <option value="0">本地 Windows 语音</option>
            <option value="1">Edge 在线语音</option>
          </select>
        </div>
        <div class="field">
          <label><span>Edge 在线音色</span></label>
          <select id="cfgEdgeVoice">
            <option value="0">晓晓 (zh-CN-XiaoxiaoNeural)</option>
            <option value="1">云希 (zh-CN-YunxiNeural)</option>
            <option value="2">云扬 (zh-CN-YunyangNeural)</option>
            <option value="3">晓伊 (zh-CN-XiaoyiNeural)</option>
          </select>
        </div>
        <div class="btn-row">
          <button type="button" onclick="previewSelectedVoice()">试听音色</button>
          <button type="button" onclick="loadConfigFromServer()">重新读取</button>
        </div>
        <div class="muted" id="cfgVoiceStatus"></div>
        <hr>
        <div class="card-title">静默时段与开机自启</div>
        <label class="switch"><input type="checkbox" id="cfgQuietEnabled"> 启用静默免打扰时段</label>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>开始时间</span></label><input type="time" id="cfgQuietStart"></div>
          <div class="field"><label><span>结束时间</span></label><input type="time" id="cfgQuietEnd"></div>
        </div>
        <div class="field"><label><span>NTP 校时服务器</span></label><input type="text" id="cfgNtpServer" placeholder="ntp.aliyun.com"></div>
        <label class="switch"><input type="checkbox" id="cfgAutoStart"> 开机自动启动</label>
      </div>
    </div>

    <!-- 自定义闹钟管理卡片 -->
    <div class="card">
      <div class="card-title">
        <span>自定义闹钟</span>
        <div class="btn-row">
          <button type="button" class="chip" onclick="addPresetAlarm('water')">+ 喝水提醒</button>
          <button type="button" class="chip" onclick="addPresetAlarm('stand')">+ 起身活动</button>
          <button type="button" class="chip" onclick="addPresetAlarm('offwork')">+ 下班打卡</button>
          <button type="button" class="primary" onclick="openAlarmEditor(-1)">+ 新建闹钟</button>
        </div>
      </div>

      <div id="alarmEditorBox" class="alarm-editor" style="display:none">
        <div class="card-title">
          <span id="alarmEditorTitle">新建闹钟</span>
          <div class="btn-row">
            <button type="button" class="chip" onclick="testEditingAlarm()">试听此闹钟</button>
            <button type="button" class="chip" onclick="closeAlarmEditor()">取消编辑</button>
            <button type="button" class="primary" onclick="commitAlarmEditor()">应用到列表</button>
          </div>
        </div>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>闹钟名称</span></label><input type="text" id="aeLabel" placeholder="例如：喝水提醒"></div>
          <div class="grid2" style="gap:10px">
            <div class="field"><label><span>响铃时间</span></label><input type="time" id="aeTime" value="10:00"></div>
            <div class="field"><label><span>播放次数 (1–10)</span></label><input type="number" id="aeRepeat" min="1" max="10" value="1"></div>
          </div>
        </div>
        <div class="grid2" style="gap:10px">
          <div class="field">
            <label><span>重复周期</span></label>
            <select id="aeMode" onchange="onAlarmModeChange()">
              <option value="once">仅一次</option>
              <option value="daily">每天</option>
              <option value="workdays">工作日</option>
              <option value="weekends">周末</option>
              <option value="weekly">自定义星期</option>
              <option value="interval_days">每隔 N 天</option>
            </select>
          </div>
          <div class="field" id="aeOnceWrap" style="display:none">
            <label><span>目标日期（可留空）</span></label>
            <input type="date" id="aeOnceDate">
          </div>
          <div class="field" id="aeIntervalWrap" style="display:none">
            <label><span>每隔天数 (1–365) / 起始日期</span></label>
            <div style="display:flex;gap:8px">
              <input type="number" id="aeIntervalDays" min="1" max="365" value="2" style="max-width:96px">
              <input type="date" id="aeAnchorDate">
            </div>
          </div>
        </div>
        <div id="aeWeeklyWrap" class="field" style="display:none">
          <label><span>每周日期</span></label>
          <div class="wd-row">
            <label><input type="checkbox" id="aeWd0"> 周一</label>
            <label><input type="checkbox" id="aeWd1"> 周二</label>
            <label><input type="checkbox" id="aeWd2"> 周三</label>
            <label><input type="checkbox" id="aeWd3"> 周四</label>
            <label><input type="checkbox" id="aeWd4"> 周五</label>
            <label><input type="checkbox" id="aeWd5"> 周六</label>
            <label><input type="checkbox" id="aeWd6"> 周日</label>
          </div>
        </div>
        <div class="btn-row">
          <label class="switch"><input type="checkbox" id="aePlayChime" checked> 播放提示音</label>
          <label class="switch"><input type="checkbox" id="aeTtsEnabled" checked> 文字转语音播报</label>
          <label class="switch"><input type="checkbox" id="aeShowAlert"> 响铃时弹出提醒窗口（默认不弹）</label>
          <label class="switch"><input type="checkbox" id="aeRespectQuiet"> 静默时段跳过</label>
        </div>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>播报文字（{TIME} {LABEL} {TIME12}）</span></label><input type="text" id="aeTtsText" placeholder="现在时间，{TIME}，{LABEL}"></div>
          <div class="field"><label><span>自定义音频路径（可选）</span></label><input type="text" id="aeCustomAudio" placeholder="例如 D:\Audio\alarm.mp3"></div>
        </div>
      </div>

      <div id="alarmListBox" class="alarm-list"></div>
      <div id="alarmDirtyHint" class="muted" style="display:none;color:var(--accent);font-weight:700;margin-top:8px">自定义闹钟有未保存修改，请点击下方「保存配置并生效」写入磁盘</div>
    </div>

    <div class="card">
      <div class="row" style="justify-content:flex-end">
        <button class="primary" onclick="saveAllConfig()">保存配置并生效</button>
      </div>
    </div>
  </div>

  <!-- Tab 3: 版本与自动更新 -->
  <div id="tab-updates" class="panel">
    <div class="card">
      <div class="card-title">
        <span>软件版本与自动更新</span>
        <span class="mono ver" id="updCurVer">--</span>
      </div>
      <div class="row"><span class="muted">检查状态</span><span id="updStatusText" style="text-align:right">--</span></div>
      <div class="row" id="updFoundRow" style="display:none">
        <span class="muted">发现新版本</span>
        <span class="mono" style="color:var(--accent);font-weight:700" id="updNewVer">--</span>
      </div>
      <div class="field" id="updNotesBox" style="display:none">
        <label><span>更新内容说明</span></label>
        <div class="log-box" style="height:auto;max-height:180px" id="updNotesText"></div>
      </div>
      <div class="row" id="updDlRow" style="display:none">
        <span class="muted" id="updDlStatus">准备下载...</span>
        <span class="mono" id="updDlPct">0%</span>
      </div>
      <div class="btn-row">
        <button class="primary" id="btnCheckUpd" onclick="postAction('/api/action/check-update')">立即检查更新</button>
        <button id="btnApplyUpd" style="display:none" onclick="postAction('/api/action/apply-update')">一键下载并重启更新</button>
      </div>
    </div>

    <div class="card">
      <div class="card-title">更新源配置</div>
      <div class="field">
        <label><span>更新源地址（Gitee / GitHub / version.json）</span></label>
        <input type="text" id="cfgUpdUrl" placeholder="https://gitee.com/djjz/HalfHourChime/releases/latest">
      </div>
      <div class="grid2" style="gap:10px">
        <div class="field">
          <label><span>访问令牌 Token（可选）</span><span class="muted" id="tokenStateBadge">未设置</span></label>
          <input type="password" id="cfgGhToken" placeholder="留空保持不变">
        </div>
        <div class="field">
          <label><span>检查周期（小时，1–168）</span></label>
          <input type="number" id="cfgUpdInterval" min="1" max="168">
        </div>
      </div>
      <div class="btn-row">
        <button class="primary" onclick="saveUpdateSettings(false)">保存配置</button>
        <button onclick="saveUpdateSettings(true)">清除 Token</button>
      </div>
    </div>
  </div>

  <!-- Tab 4: 实时运行日志 -->
  <div id="tab-logs" class="panel">
    <div class="card">
      <div class="card-title">
        <span>实时运行日志（最近 200 行）</span>
        <div class="btn-row">
          <button class="chip" onclick="loadLogs()">立即刷新</button>
          <button class="chip" onclick="postAction('/api/action/open-logs')">打开本机日志目录</button>
        </div>
      </div>
      <div class="row" style="flex-wrap:wrap">
        <div class="btn-row" style="flex:1">
          <select id="logLevel" style="width:auto" onchange="loadLogs()">
            <option value="ALL">全部级别</option>
            <option value="INFO">INFO</option>
            <option value="WARN">WARN</option>
            <option value="ERROR">ERROR</option>
            <option value="DEBUG">DEBUG</option>
          </select>
          <input type="text" id="logKw" style="max-width:260px" placeholder="按关键字过滤（如 update / tts / ntp）" oninput="loadLogs()">
        </div>
        <label class="switch"><input type="checkbox" id="logAutoRefresh" checked> 自动实时刷新 (3秒)</label>
      </div>
      <div class="muted mono" id="logFilePath">文件：--</div>
      <div class="log-box" id="logBox">正在加载日志...</div>
    </div>
  </div>

  <div class="toast-bar">
    <span id="toastText">本地管理控制台已就绪</span>
    <span class="muted mono" id="connState">已连接 127.0.0.1:3000</span>
  </div>
</div>

<script>
let activeTab = 'overview';
let volumeDebounce = null;
let draftAlarms = [];
let editingAlarmIdx = -2; // -2: closed, -1: new, >=0: existing index
let maxKnownAlarmId = 0;
let alarmsDirty = false;

function setAlarmsDirty(flag){
  alarmsDirty = !!flag;
  const el = document.getElementById('alarmDirtyHint');
  if(el) el.style.display = alarmsDirty ? 'block' : 'none';
}

function pad2(n){ return String(Math.max(0, Number(n)||0)).padStart(2,'0'); }
function todayStr(){
  const d = new Date();
  return d.getFullYear() + '-' + pad2(d.getMonth()+1) + '-' + pad2(d.getDate());
}
function showToast(msg, isErr){
  const el = document.getElementById('toastText');
  el.textContent = msg;
  el.style.color = isErr ? 'var(--err)' : 'var(--ok)';
}
function switchTab(id, btn){
  activeTab = id;
  document.querySelectorAll('.nav button').forEach(b=>b.classList.remove('active'));
  if(btn) btn.classList.add('active');
  document.querySelectorAll('.panel').forEach(p=>p.classList.remove('active'));
  document.getElementById('tab-'+id).classList.add('active');
  if((id === 'settings' && !alarmsDirty && editingAlarmIdx === -2) || id === 'updates'){
    loadConfigFromServer();
  }
  if(id === 'logs') loadLogs();
}

async function postAction(url, body){
  try{
    const opt = { method:'POST', headers:{'Content-Type':'application/json'} };
    if(body !== undefined) opt.body = JSON.stringify(body);
    const r = await fetch(url, opt);
    const j = await r.json();
    if(r.ok && j.ok){
      showToast(j.message || '操作成功', false);
      setTimeout(refreshStatus, 180);
      return true;
    }else{
      showToast(j.error || '操作失败', true);
      setTimeout(refreshStatus, 180);
      return false;
    }
  }catch(e){
    showToast('请求失败，请确认软件仍在运行', true);
    return false;
  }
}

function onVolumeSlide(val){
  document.getElementById('volLabel').textContent = val + '%';
  clearTimeout(volumeDebounce);
  volumeDebounce = setTimeout(()=>{
    postAction('/api/action/set-volume', { volume: Number(val) });
  }, 120);
}

function setTheme(idx){
  document.body.setAttribute('data-theme', String(idx));
  postAction('/api/action/set-theme', { theme_index: idx });
}

function previewSelectedVoice(){
  postAction('/api/action/test-voice', {
    tts_engine_index: Number(document.getElementById('cfgTtsEngine').value),
    edge_voice_index: Number(document.getElementById('cfgEdgeVoice').value),
    tts_rate: Number(document.getElementById('cfgTtsRate').value || 0),
  });
}

function describeAlarmCycle(a){
  const m = a.repeat_mode || 'daily';
  if(m === 'once') return '仅一次' + (a.once_date ? ' ('+a.once_date+')' : '');
  if(m === 'daily') return '每天';
  if(m === 'workdays') return '工作日 (周一至五)';
  if(m === 'weekends') return '周末 (周六/日)';
  if(m === 'weekly'){
    const names = ['一','二','三','四','五','六','日'];
    const arr = (a.weekdays||[]).map((on,i)=>on?names[i]:null).filter(Boolean);
    return arr.length ? '每周 ' + arr.join(' ') : '每周 (未选)';
  }
  if(m === 'interval_days') return '每 ' + Math.max(1, Number(a.interval_days)||1) + ' 天';
  return '每天';
}

function describeAlarmAudio(a){
  const parts = [];
  if(a.play_chime_before) parts.push('提示音');
  if((a.custom_audio_path||'').trim()) parts.push('自定义音频');
  if(a.tts_enabled) parts.push('文字转语音');
  if(a.show_alert) parts.push('弹窗');
  return parts.length ? parts.join(' + ') : '仅提示';
}

function escapeHtml(s){
  return String(s||'').replace(/[&<>"']/g, c=>({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]));
}

function renderAlarms(){
  const box = document.getElementById('alarmListBox');
  if(!draftAlarms.length){
    box.innerHTML = '<div class="muted" style="padding:10px 0">暂无自定义闹钟，可点击上方“+ 新建闹钟”或快速添加模板。</div>';
    return;
  }
  box.innerHTML = draftAlarms.map((a, idx)=>`
    <div class="alarm-item">
      <div class="alarm-item-left">
        <input type="checkbox" ${a.enabled ? 'checked' : ''} onchange="toggleAlarmEnabled(${idx}, this.checked)">
        <span class="alarm-time">${pad2(a.hour)}:${pad2(a.minute)}</span>
        <div>
          <div style="font-weight:700">${escapeHtml(a.label)}</div>
          <div class="muted">${escapeHtml(describeAlarmCycle(a))} · ${escapeHtml(describeAlarmAudio(a))}</div>
        </div>
      </div>
      <div class="btn-row">
        <button type="button" class="chip" onclick="testSavedOrDraftAlarm(${idx})">试听</button>
        <button type="button" class="chip" onclick="openAlarmEditor(${idx})">编辑</button>
        <button type="button" class="chip danger" onclick="deleteAlarm(${idx})">删除</button>
      </div>
    </div>
  `).join('');
}

function toggleAlarmEnabled(idx, checked){
  if(draftAlarms[idx]){
    draftAlarms[idx].enabled = !!checked;
    setAlarmsDirty(true);
  }
}

function deleteAlarm(idx){
  draftAlarms.splice(idx, 1);
  if(editingAlarmIdx === idx) closeAlarmEditor();
  else if(editingAlarmIdx > idx) editingAlarmIdx -= 1;
  renderAlarms();
  setAlarmsDirty(true);
}

function nextLocalAlarmId(){
  maxKnownAlarmId = draftAlarms.reduce((m,a)=>Math.max(m, Number(a.id)||0), maxKnownAlarmId) + 1;
  return maxKnownAlarmId;
}

function addPresetAlarm(kind){
  const id = nextLocalAlarmId();
  let item;
  if(kind === 'water'){
    item = {
      id, label:'喝水提醒', enabled:true, hour:10, minute:30,
      repeat_mode:'daily', weekdays:[true,true,true,true,true,true,true],
      interval_days:1, anchor_date:'', once_date:'', repeat_count:1,
      play_chime_before:true, tts_enabled:true, tts_text:'现在时间，{TIME}，请喝一杯水休息一下',
      custom_audio_path:'', respect_quiet_hours:false, show_alert:false
    };
  }else if(kind === 'stand'){
    item = {
      id, label:'起身活动', enabled:true, hour:15, minute:0,
      repeat_mode:'workdays', weekdays:[true,true,true,true,true,false,false],
      interval_days:1, anchor_date:'', once_date:'', repeat_count:1,
      play_chime_before:true, tts_enabled:true, tts_text:'现在时间，{TIME}，久坐辛苦了，起来活动几分钟吧',
      custom_audio_path:'', respect_quiet_hours:false, show_alert:false
    };
  }else{
    item = {
      id, label:'下班打卡提醒', enabled:true, hour:18, minute:0,
      repeat_mode:'workdays', weekdays:[true,true,true,true,true,false,false],
      interval_days:1, anchor_date:'', once_date:'', repeat_count:2,
      play_chime_before:true, tts_enabled:true, tts_text:'现在时间，{TIME}，请记得下班打卡',
      custom_audio_path:'', respect_quiet_hours:false, show_alert:false
    };
  }
  draftAlarms.push(item);
  renderAlarms();
  setAlarmsDirty(true);
  openAlarmEditor(draftAlarms.length - 1);
}

function onAlarmModeChange(){
  const m = document.getElementById('aeMode').value;
  document.getElementById('aeOnceWrap').style.display = m === 'once' ? 'grid' : 'none';
  document.getElementById('aeWeeklyWrap').style.display = m === 'weekly' ? 'grid' : 'none';
  document.getElementById('aeIntervalWrap').style.display = m === 'interval_days' ? 'grid' : 'none';
}

function openAlarmEditor(idx){
  editingAlarmIdx = idx;
  const a = idx >= 0 && draftAlarms[idx] ? draftAlarms[idx] : {
    id: nextLocalAlarmId(),
    label: '日常提醒',
    enabled: true,
    hour: 9,
    minute: 0,
    repeat_mode: 'daily',
    weekdays: [true,true,true,true,true,false,false],
    interval_days: 2,
    anchor_date: todayStr(),
    once_date: '',
    repeat_count: 1,
    play_chime_before: true,
    tts_enabled: true,
    tts_text: '现在时间，{TIME}，{LABEL}',
    custom_audio_path: '',
    respect_quiet_hours: false,
    show_alert: false,
  };
  document.getElementById('alarmEditorTitle').textContent = idx < 0 ? '新建自定义闹钟' : '编辑自定义闹钟';
  document.getElementById('aeLabel').value = a.label || '';
  document.getElementById('aeTime').value = pad2(a.hour) + ':' + pad2(a.minute);
  document.getElementById('aeRepeat').value = a.repeat_count ?? 1;
  document.getElementById('aeMode').value = a.repeat_mode || 'daily';
  document.getElementById('aeOnceDate').value = a.once_date || '';
  document.getElementById('aeIntervalDays').value = a.interval_days || 2;
  document.getElementById('aeAnchorDate').value = a.anchor_date || todayStr();
  const wds = a.weekdays || [true,true,true,true,true,false,false];
  for(let i=0;i<7;i++) document.getElementById('aeWd'+i).checked = !!wds[i];
  document.getElementById('aePlayChime').checked = !!a.play_chime_before;
  document.getElementById('aeTtsEnabled').checked = !!a.tts_enabled;
  document.getElementById('aeTtsText').value = a.tts_text || '';
  document.getElementById('aeCustomAudio').value = a.custom_audio_path || '';
  document.getElementById('aeShowAlert').checked = !!a.show_alert;
  document.getElementById('aeRespectQuiet').checked = !!a.respect_quiet_hours;
  onAlarmModeChange();
  document.getElementById('alarmEditorBox').style.display = 'grid';
}

function closeAlarmEditor(){
  editingAlarmIdx = -2;
  document.getElementById('alarmEditorBox').style.display = 'none';
}

function readAlarmFromEditor(){
  const [h, m] = parseTimeStr(document.getElementById('aeTime').value, 9, 0);
  const base = (editingAlarmIdx >= 0 && draftAlarms[editingAlarmIdx]) ? draftAlarms[editingAlarmIdx] : null;
  const weekdays = [];
  for(let i=0;i<7;i++) weekdays.push(document.getElementById('aeWd'+i).checked);
  return {
    id: base ? base.id : nextLocalAlarmId(),
    label: document.getElementById('aeLabel').value.trim() || ('闹钟 ' + pad2(h) + ':' + pad2(m)),
    enabled: base ? !!base.enabled : true,
    hour: h,
    minute: m,
    repeat_mode: document.getElementById('aeMode').value,
    weekdays,
    interval_days: Math.min(365, Math.max(1, Number(document.getElementById('aeIntervalDays').value || 1))),
    anchor_date: document.getElementById('aeAnchorDate').value.trim(),
    once_date: document.getElementById('aeOnceDate').value.trim(),
    repeat_count: Math.min(10, Math.max(1, Number(document.getElementById('aeRepeat').value || 1))),
    play_chime_before: document.getElementById('aePlayChime').checked,
    tts_enabled: document.getElementById('aeTtsEnabled').checked,
    tts_text: document.getElementById('aeTtsText').value.trim(),
    custom_audio_path: document.getElementById('aeCustomAudio').value.trim(),
    respect_quiet_hours: document.getElementById('aeRespectQuiet').checked,
    show_alert: document.getElementById('aeShowAlert').checked,
  };
}

function commitAlarmEditor(){
  const item = readAlarmFromEditor();
  if(editingAlarmIdx >= 0 && draftAlarms[editingAlarmIdx]){
    draftAlarms[editingAlarmIdx] = item;
  }else{
    draftAlarms.push(item);
  }
  closeAlarmEditor();
  renderAlarms();
  setAlarmsDirty(true);
  showToast('已更新闹钟列表，请点击下方「保存配置并生效」写入磁盘', false);
}

function testEditingAlarm(){
  postAction('/api/action/test-alarm', { alarm: readAlarmFromEditor() });
}

function testSavedOrDraftAlarm(idx){
  const a = draftAlarms[idx];
  if(a) postAction('/api/action/test-alarm', { alarm: a });
}

async function loadConfigFromServer(){
  try{
    const r = await fetch('/api/config');
    const c = await r.json();
    document.getElementById('cfgHourlyEnabled').checked = !!c.hourly_chime_enabled;
    document.getElementById('cfgHalfHourEnabled').checked = !!c.half_hour_chime_enabled;
    document.getElementById('cfgExtraMinutes').value = c.custom_chime_minutes_str || '';
    document.getElementById('cfgChimeRepeat').value = c.chime_repeat_count ?? 1;
    document.getElementById('cfgTtsRate').value = c.tts_rate ?? 0;
    document.getElementById('cfgPlaySound').checked = !!c.chime_play_sound;
    document.getElementById('cfgChimeTts').checked = !!c.chime_tts_enabled;
    document.getElementById('cfgChimeTpl').value = c.chime_tts_template || '';
    document.getElementById('cfgTtsEngine').value = String(c.tts_engine_index ?? 1);
    document.getElementById('cfgEdgeVoice').value = String(c.edge_voice_index ?? 0);
    document.getElementById('cfgQuietEnabled').checked = !!c.quiet_enabled;
    document.getElementById('cfgQuietStart').value = pad2(c.quiet_start_hour)+':'+pad2(c.quiet_start_minute);
    document.getElementById('cfgQuietEnd').value = pad2(c.quiet_end_hour)+':'+pad2(c.quiet_end_minute);
    document.getElementById('cfgNtpServer').value = c.ntp_server || 'ntp.aliyun.com';
    document.getElementById('cfgAutoStart').checked = !!c.auto_start;
    document.getElementById('cfgHolidayEnabled').checked = !!c.holiday_enabled;
    document.getElementById('cfgHolidayTpl').value = c.holiday_greeting_template || '';
    document.getElementById('cfgHolidayAudio').value = c.holiday_custom_audio_path || '';

    draftAlarms = Array.isArray(c.alarms) ? JSON.parse(JSON.stringify(c.alarms)) : [];
    maxKnownAlarmId = draftAlarms.reduce((m,a)=>Math.max(m, Number(a.id)||0), maxKnownAlarmId);
    setAlarmsDirty(false);
    closeAlarmEditor();
    renderAlarms();

    document.getElementById('cfgUpdUrl').value = c.update_check_url || '';
    document.getElementById('cfgUpdInterval').value = c.update_check_interval_hours ?? 6;
    document.getElementById('tokenStateBadge').textContent = c.has_github_token ? '已配置 Token' : '未设置';
  }catch(e){
    showToast('读取配置失败', true);
  }
}

function parseTimeStr(val, defH, defM){
  const parts = String(val||'').split(':');
  if(parts.length === 2){
    const h = Math.min(23, Math.max(0, parseInt(parts[0],10)||0));
    const m = Math.min(59, Math.max(0, parseInt(parts[1],10)||0));
    return [h, m];
  }
  return [defH, defM];
}

async function saveAllConfig(){
  const alarmsToSave = JSON.parse(JSON.stringify(draftAlarms));
  if(editingAlarmIdx !== -2){
    const item = readAlarmFromEditor();
    if(editingAlarmIdx >= 0 && alarmsToSave[editingAlarmIdx]) alarmsToSave[editingAlarmIdx] = item;
    else alarmsToSave.push(item);
  }
  const [qsH, qsM] = parseTimeStr(document.getElementById('cfgQuietStart').value, 0, 0);
  const [qeH, qeM] = parseTimeStr(document.getElementById('cfgQuietEnd').value, 8, 0);
  const payload = {
    hourly_chime_enabled: document.getElementById('cfgHourlyEnabled').checked,
    half_hour_chime_enabled: document.getElementById('cfgHalfHourEnabled').checked,
    custom_chime_minutes_str: document.getElementById('cfgExtraMinutes').value,
    chime_repeat_count: Number(document.getElementById('cfgChimeRepeat').value || 1),
    tts_rate: Number(document.getElementById('cfgTtsRate').value || 0),
    chime_play_sound: document.getElementById('cfgPlaySound').checked,
    chime_tts_enabled: document.getElementById('cfgChimeTts').checked,
    chime_tts_template: document.getElementById('cfgChimeTpl').value,
    tts_engine_index: Number(document.getElementById('cfgTtsEngine').value),
    edge_voice_index: Number(document.getElementById('cfgEdgeVoice').value),
    quiet_enabled: document.getElementById('cfgQuietEnabled').checked,
    quiet_start_hour: qsH, quiet_start_minute: qsM,
    quiet_end_hour: qeH, quiet_end_minute: qeM,
    ntp_server: document.getElementById('cfgNtpServer').value,
    auto_start: document.getElementById('cfgAutoStart').checked,
    holiday_enabled: document.getElementById('cfgHolidayEnabled').checked,
    holiday_greeting_template: document.getElementById('cfgHolidayTpl').value,
    holiday_custom_audio_path: document.getElementById('cfgHolidayAudio').value,
    alarms: alarmsToSave,
  };
  const ok = await postAction('/api/config', payload);
  if(ok){
    draftAlarms = alarmsToSave;
    closeAlarmEditor();
    setAlarmsDirty(false);
    await loadConfigFromServer();
  }
}

async function saveUpdateSettings(clearToken){
  const tok = document.getElementById('cfgGhToken').value.trim();
  const payload = {
    update_check_url: document.getElementById('cfgUpdUrl').value.trim(),
    update_check_interval_hours: Number(document.getElementById('cfgUpdInterval').value || 6),
    clear_github_token: !!clearToken,
  };
  if(!clearToken && tok){
    payload.update_github_token = tok;
  }
  await postAction('/api/config', payload);
  document.getElementById('cfgGhToken').value = '';
  await loadConfigFromServer();
}

async function refreshStatus(){
  try{
    const r = await fetch('/api/status');
    const d = await r.json();
    document.getElementById('connState').textContent = '已连接 127.0.0.1:3000';
    document.getElementById('ver').textContent = d.version || '--';
    document.getElementById('updCurVer').textContent = d.version || '--';
    document.body.setAttribute('data-theme', String(d.theme_index ?? 2));
    document.querySelectorAll('#themeBtns button').forEach(b=>{
      b.classList.toggle('on', Number(b.dataset.themeIdx) === (d.theme_index ?? 2));
    });

    document.getElementById('clock').textContent = d.current_time || '--';
    document.getElementById('date').textContent = d.current_date || '--';
    const sb = document.getElementById('stateBadge');
    sb.textContent = d.mute_mode ? '全局已静音' : (d.is_quiet_now ? '静默时段中' : '正常运行中');
    sb.style.color = (d.mute_mode || d.is_quiet_now) ? 'var(--warn)' : 'var(--ok)';
    document.getElementById('quietText').textContent = '静默设置：' + (d.quiet_summary || '--');
    document.getElementById('calendarText').textContent = d.today_calendar_preview || '';
    document.getElementById('nextTitle').textContent = d.next_event_title || '--';
    document.getElementById('nextTime').textContent = d.next_event_time || '--';
    document.getElementById('countdown').textContent = d.next_event_countdown || '--';

    document.getElementById('muteBtn').textContent = d.mute_mode ? '取消静音模式' : '开启静音模式';
    document.getElementById('stopAudioBtn').textContent = d.is_audio_playing ? '停止当前播报 (正在播放)' : '停止当前播报';
    document.getElementById('ntpBtn').disabled = !!d.is_syncing;
    const slider = document.getElementById('liveVol');
    if(document.activeElement !== slider && d.volume !== undefined){
      slider.value = d.volume;
      document.getElementById('volLabel').textContent = d.volume + '%';
    }
    document.getElementById('chimeAlarmStat').textContent = (d.chime_minutes || '--') + ' · 闹钟 ' + (d.enabled_alarms_count ?? 0) + ' 个';
    document.getElementById('curVoice').textContent = (d.tts_engine||'') + (d.edge_tts_voice ? ' · '+d.edge_tts_voice : '');
    document.getElementById('ntpStatus').textContent = d.ntp_status || '--';
    document.getElementById('cfgPath').textContent = d.config_path || '--';
    document.getElementById('mWs').textContent = (d.memory_ws_mb ?? 0).toFixed(1) + ' MB';
    document.getElementById('mPriv').textContent = (d.memory_private_mb ?? 0).toFixed(1) + ' MB';
    document.getElementById('mCpu').textContent = (d.process_cpu_pct ?? 0).toFixed(1) + '% / ' + (d.system_cpu_pct ?? 0).toFixed(0) + '%';
    document.getElementById('mUp').textContent = d.uptime_text || '--';
    if(d.tts_preview_status){
      document.getElementById('ttsStatusLine').textContent = '语音状态：' + d.tts_preview_status;
      document.getElementById('cfgVoiceStatus').textContent = d.tts_preview_status;
    }

    document.getElementById('btnCheckUpd').disabled = !!d.update_checking;
    document.getElementById('btnCheckUpd').textContent = d.update_checking ? '正在检查中...' : '立即检查更新';
    document.getElementById('updStatusText').textContent = d.update_status_text || '--';
    document.getElementById('updFoundRow').style.display = d.has_update ? 'flex' : 'none';
    document.getElementById('updNotesBox').style.display = (d.has_update && d.update_notes) ? 'grid' : 'none';
    document.getElementById('btnApplyUpd').style.display = d.has_update ? 'inline-block' : 'none';
    document.getElementById('updNewVer').textContent = d.update_version || '';
    document.getElementById('updNotesText').textContent = d.update_notes || '';
    document.getElementById('updDlRow').style.display = d.update_downloading ? 'flex' : 'none';
    document.getElementById('updDlPct').textContent = (d.update_progress ?? 0) + '%';
    document.getElementById('updDlStatus').textContent = d.update_download_status || '';
  }catch(e){
    document.getElementById('connState').textContent = '连接断开';
  }
}

async function loadLogs(){
  try{
    const lvl = encodeURIComponent(document.getElementById('logLevel').value);
    const kw = encodeURIComponent(document.getElementById('logKw').value.trim());
    const r = await fetch(`/api/logs?level=${lvl}&q=${kw}`);
    const d = await r.json();
    document.getElementById('logFilePath').textContent = '日志文件：' + (d.path || '--');
    const box = document.getElementById('logBox');
    box.innerHTML = '';
    (d.lines || []).forEach(line=>{
      const div = document.createElement('div');
      div.textContent = line;
      if(line.includes('[ERROR]')) div.className = 'log-err';
      else if(line.includes('[WARN')) div.className = 'log-warn';
      else if(line.includes('[DEBUG]')) div.className = 'log-dbg';
      else div.className = 'log-info';
      box.appendChild(div);
    });
    box.scrollTop = box.scrollHeight;
  }catch(e){}
}

loadConfigFromServer();
refreshStatus();
setInterval(refreshStatus, 1000);
setInterval(()=>{
  if(activeTab === 'logs' && document.getElementById('logAutoRefresh').checked){
    loadLogs();
  }
}, 3000);
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_content_length_and_query() {
        let headers = "POST /api/config HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nContent-Length: 42\r\n";
        assert_eq!(parse_content_length(headers), 42);
        assert_eq!(
            extract_query_param("level=WARN&q=Edge+TTS%20404", "q"),
            "Edge TTS 404"
        );
    }
}
