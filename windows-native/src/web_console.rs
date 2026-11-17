use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub quiet_summary: String,
    pub next_event_title: String,
    pub next_event_time: String,
    pub next_event_countdown: String,
    pub chime_minutes: String,
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
    pub holiday_feed_status: String,
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
    pub chime_minutes_str: String,
    pub chime_repeat_count: u8,
    pub chime_play_sound: bool,
    pub chime_tts_enabled: bool,
    pub chime_tts_template: String,
    pub holiday_enabled: bool,
    pub holiday_greeting_template: String,
    pub holiday_custom_rules: String,
    pub holiday_data_source_url: String,
    pub holiday_custom_audio_path: String,
    pub quiet_enabled: bool,
    pub quiet_start_hour: u8,
    pub quiet_start_minute: u8,
    pub quiet_end_hour: u8,
    pub quiet_end_minute: u8,
    pub meal_enabled: bool,
    pub meal_hour: u8,
    pub meal_minute: u8,
    pub meal_repeat_count: u8,
    pub meal_play_chime_before_custom: bool,
    pub meal_custom_audio_path: String,
    pub meal_tts_enabled: bool,
    pub meal_tts_text: String,
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
    pub chime_minutes_str: Option<String>,
    pub chime_repeat_count: Option<u8>,
    pub chime_play_sound: Option<bool>,
    pub chime_tts_enabled: Option<bool>,
    pub chime_tts_template: Option<String>,
    pub holiday_enabled: Option<bool>,
    pub holiday_greeting_template: Option<String>,
    pub holiday_custom_rules: Option<String>,
    pub holiday_data_source_url: Option<String>,
    pub holiday_custom_audio_path: Option<String>,
    pub quiet_enabled: Option<bool>,
    pub quiet_start_hour: Option<u8>,
    pub quiet_start_minute: Option<u8>,
    pub quiet_end_hour: Option<u8>,
    pub quiet_end_minute: Option<u8>,
    pub meal_enabled: Option<bool>,
    pub meal_hour: Option<u8>,
    pub meal_minute: Option<u8>,
    pub meal_repeat_count: Option<u8>,
    pub meal_play_chime_before_custom: Option<bool>,
    pub meal_custom_audio_path: Option<String>,
    pub meal_tts_enabled: Option<bool>,
    pub meal_tts_text: Option<String>,
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
    TestVoice {
        tts_engine_index: Option<i32>,
        edge_voice_index: Option<i32>,
        tts_rate: Option<i8>,
    },
    TestMeal,
    ToggleMute,
    SyncNtp {
        server: Option<String>,
    },
    SetVolume(u8),
    SetTheme(i32),
    TrimMemory(Sender<Result<String, String>>),
    RefreshHolidays(Sender<Result<String, String>>),
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
                            handle_http_client(stream, &snap_c, &cfg_c, &tx_c);
                        }
                        Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(60));
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
                    return len.min(128 * 1024);
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
        ("POST", "/api/action/test-meal") => {
            let _ = action_tx.send(WebAction::TestMeal);
            let body = json_ok("已触发餐饮/定时提醒测试");
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
        ("POST", "/api/action/refresh-holidays") => {
            send_sync_action(&mut stream, action_tx, WebAction::RefreshHolidays);
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
<title>半整点提醒 · 本地运维控制台</title>
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
  .wrap{max-width:960px;margin:0 auto;display:grid;gap:14px}
  .topbar{display:flex;flex-wrap:wrap;justify-content:space-between;align-items:center;gap:12px}
  .brand{display:flex;align-items:baseline;gap:10px}
  .brand h1{margin:0;font-size:20px;font-weight:700}
  .ver{font-family:Consolas,monospace;color:var(--accent);font-size:13px;font-weight:700}
  .nav{display:flex;flex-wrap:wrap;gap:6px;background:var(--surface2);padding:4px;border-radius:10px;border:1px solid var(--border)}
  .nav button{border:none;background:transparent;color:var(--muted);padding:7px 14px;border-radius:7px;font-size:13px;font-weight:600;cursor:pointer}
  .nav button.active{background:var(--card);color:var(--text);box-shadow:0 1px 2px rgba(0,0,0,.08)}
  .panel{display:none;gap:14px}
  .panel.active{display:grid}
  .grid2{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:14px}
  .card{background:var(--card);border:1px solid var(--border);border-radius:10px;padding:16px 18px;display:grid;gap:12px}
  .card-title{font-size:14px;font-weight:700;color:var(--text);display:flex;justify-content:space-between;align-items:center}
  .clock{font-family:Consolas,monospace;font-size:38px;font-weight:700;letter-spacing:1px;line-height:1.1}
  .row{display:flex;justify-content:space-between;align-items:center;gap:12px;font-size:13px}
  .muted{color:var(--muted);font-size:12px}
  .mono{font-family:Consolas,monospace}
  .btn-row{display:flex;flex-wrap:wrap;gap:8px}
  button{border:1px solid var(--border);background:var(--card);color:var(--text);padding:8px 13px;border-radius:8px;font-size:13px;font-weight:600;cursor:pointer}
  button:hover{border-color:var(--accent)}
  button:disabled{opacity:.55;cursor:not-allowed}
  button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
  button.chip{padding:5px 10px;font-size:12px;border-radius:6px;background:var(--surface2)}
  button.chip.on{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
  .field{display:grid;gap:5px}
  .field label{font-size:12px;color:var(--sub);font-weight:600;display:flex;justify-content:space-between;align-items:center}
  input[type="text"],input[type="password"],input[type="number"],input[type="time"],select,textarea{
    width:100%;padding:8px 10px;border-radius:7px;border:1px solid var(--border);
    background:var(--surface2);color:var(--text);font-size:13px;font-family:inherit;
  }
  textarea{resize:vertical;min-height:66px;font-family:Consolas,monospace;font-size:12px}
  .switch{display:flex;align-items:center;gap:8px;font-size:13px;cursor:pointer;user-select:none}
  .switch input{width:16px;height:16px;accent-color:var(--accent);cursor:pointer}
  .kv-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(165px,1fr));gap:10px}
  .metric{background:var(--surface2);border:1px solid var(--border);border-radius:8px;padding:10px 12px}
  .metric .k{font-size:11px;color:var(--muted)}
  .metric .v{font-size:16px;font-weight:700;font-family:Consolas,monospace;margin-top:2px}
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
      <h1>半整点提醒 · 本地运维控制台</h1>
      <span class="ver" id="ver">--</span>
    </div>
    <div class="nav">
      <button class="active" onclick="switchTab('overview',this)">总览与控制</button>
      <button onclick="switchTab('settings',this)">报时与语音配置</button>
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
        <div class="card-title">快捷调度与实时控制</div>
        <div class="btn-row">
          <button class="primary" onclick="postAction('/api/action/test-chime')">立即测试报时</button>
          <button onclick="postAction('/api/action/test-voice',{})">试听当前音色</button>
          <button onclick="postAction('/api/action/test-meal')">测试餐饮提醒</button>
          <button onclick="postAction('/api/action/toggle-mute')" id="muteBtn">切换静音</button>
          <button onclick="postAction('/api/action/sync-ntp',{})" id="ntpBtn">立即 NTP 校时</button>
        </div>
        <div class="field">
          <label><span>全局音量（拖动实时生效）</span><span class="mono" id="volLabel">80%</span></label>
          <input type="range" id="liveVol" min="0" max="100" value="80" oninput="onVolumeSlide(this.value)">
        </div>
        <div class="field">
          <label><span>界面视觉主题（同步桌面端与网页）</span></label>
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
          <span>运行状态与性能监测</span>
          <button class="chip" onclick="postAction('/api/action/trim-memory')">释放内存</button>
        </div>
        <div class="kv-grid">
          <div class="metric"><div class="k">工作集内存</div><div class="v" id="mWs">-- MB</div></div>
          <div class="metric"><div class="k">私有内存</div><div class="v" id="mPriv">-- MB</div></div>
          <div class="metric"><div class="k">本进程 / 整机 CPU</div><div class="v" id="mCpu">-- / --</div></div>
          <div class="metric"><div class="k">运行时长</div><div class="v" id="mUp">--</div></div>
        </div>
        <div class="row"><span class="muted">当前语音</span><span id="curVoice">--</span></div>
        <div class="row"><span class="muted">校时状态</span><span id="ntpStatus">--</span></div>
        <div class="row"><span class="muted">配置文件</span><span class="mono muted" id="cfgPath" style="text-align:right;word-break:break-all">--</span></div>
      </div>
    </div>
  </div>

  <!-- Tab 2: 报时与语音配置 -->
  <div id="tab-settings" class="panel">
    <div class="grid2">
      <div class="card">
        <div class="card-title">半/整点报时规则</div>
        <div class="field">
          <label>
            <span>报时分钟（0–59，逗号分隔）</span>
            <span class="btn-row">
              <button type="button" class="chip" onclick="setMinutesPreset('0')">整点</button>
              <button type="button" class="chip" onclick="setMinutesPreset('0, 30')">半整点</button>
              <button type="button" class="chip" onclick="setMinutesPreset('0, 15, 30, 45')">每15分</button>
            </span>
          </label>
          <input type="text" id="cfgMinutes" placeholder="0, 30">
        </div>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>重复播报次数 (1–10)</span></label><input type="number" id="cfgChimeRepeat" min="1" max="10"></div>
          <div class="field"><label><span>语音播报语速 (-5 ~ +5)</span></label><input type="number" id="cfgTtsRate" min="-5" max="5"></div>
        </div>
        <label class="switch"><input type="checkbox" id="cfgPlaySound"> 报时播放内置清脆提示音</label>
        <label class="switch"><input type="checkbox" id="cfgChimeTts"> 报时启用语音播报真实时间 / 自定义词</label>
        <div class="field">
          <label><span>报时语音模板（支持 {TIME} / {TIME12} / {PERIOD}）</span></label>
          <input type="text" id="cfgChimeTpl" placeholder="现在时间，{TIME}">
        </div>
      </div>

      <div class="card">
        <div class="card-title">语音引擎与在线音色</div>
        <div class="field">
          <label><span>语音合成引擎</span></label>
          <select id="cfgTtsEngine">
            <option value="1">Microsoft Edge TTS（在线神经网络语音 · 自动回退离线）</option>
            <option value="0">本地 Windows SAPI5（纯离线系统语音）</option>
          </select>
        </div>
        <div class="field">
          <label><span>Edge TTS 音色选择</span></label>
          <select id="cfgEdgeVoice">
            <option value="0">晓晓 (zh-CN-XiaoxiaoNeural · 温暖女声)</option>
            <option value="1">云希 (zh-CN-YunxiNeural · 阳光男声)</option>
            <option value="2">云扬 (zh-CN-YunyangNeural · 播音男声)</option>
            <option value="3">晓伊 (zh-CN-XiaoyiNeural · 甜美活力)</option>
          </select>
        </div>
        <div class="btn-row">
          <button type="button" onclick="previewSelectedVoice()">试听所选音色（不保存）</button>
          <button type="button" onclick="loadConfigFromServer()">从客户端重新读取</button>
        </div>
        <div class="muted" id="cfgVoiceStatus">提示：点击试听将立即打断上一段试听并播放所选音色。</div>
        <hr>
        <div class="card-title" style="margin-top:2px">静默时段与系统设置</div>
        <label class="switch"><input type="checkbox" id="cfgQuietEnabled"> 启用夜间/特定静默免打扰时段</label>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>静默开始时间</span></label><input type="time" id="cfgQuietStart"></div>
          <div class="field"><label><span>静默结束时间</span></label><input type="time" id="cfgQuietEnd"></div>
        </div>
        <div class="field"><label><span>NTP 校时服务器</span></label><input type="text" id="cfgNtpServer" placeholder="ntp.aliyun.com"></div>
        <label class="switch"><input type="checkbox" id="cfgAutoStart"> 开机自动启动并最小化到托盘</label>
      </div>

      <div class="card">
        <div class="card-title">餐饮 / 定时任务提醒</div>
        <label class="switch"><input type="checkbox" id="cfgMealEnabled"> 启用每日定时餐饮/换班提醒</label>
        <div class="grid2" style="gap:10px">
          <div class="field"><label><span>提醒时间</span></label><input type="time" id="cfgMealTime"></div>
          <div class="field"><label><span>播放次数 (1–10)</span></label><input type="number" id="cfgMealRepeat" min="1" max="10"></div>
        </div>
        <label class="switch"><input type="checkbox" id="cfgMealChimeBefore"> 播报前先响内置提示音</label>
        <label class="switch"><input type="checkbox" id="cfgMealTtsEnabled"> 启用定时文字转语音播报</label>
        <div class="field"><label><span>提醒播报文本</span></label><input type="text" id="cfgMealText"></div>
        <div class="field"><label><span>本机自定义音频路径（WAV/MP3/FLAC/OGG，可留空）</span></label><input type="text" id="cfgMealAudio" placeholder="例如 D:\Audio\meal.mp3"></div>
      </div>

      <div class="card">
        <div class="card-title">
          <span>节假日 / 周年庆专属播报</span>
          <button type="button" class="chip" onclick="postAction('/api/action/refresh-holidays')">刷新假日源</button>
        </div>
        <label class="switch"><input type="checkbox" id="cfgHolidayEnabled"> 启用农历/节假日专属问候语</label>
        <div class="field"><label><span>问候语模板（支持 {HOLIDAY} / {GREETING} / {LUNAR} / {TIME}）</span></label><input type="text" id="cfgHolidayTpl"></div>
        <div class="field"><label><span>自定义节日规则（每行一条，如 11-14=周年庆|欢迎光临 或 L01-01=新春快乐）</span></label><textarea id="cfgHolidayRules"></textarea></div>
        <div class="field"><label><span>外部节假日 JSON 数据源 URL / 本机路径</span></label><input type="text" id="cfgHolidayUrl" placeholder="可留空，使用内置公历/农历节日表"></div>
        <div class="muted" id="cfgHolidayStatus">外部源状态：--</div>
      </div>
    </div>

    <div class="card">
      <div class="row">
        <span class="muted">保存后立即写入本机 config.json 并同步更新桌面端窗口与定时调度器。</span>
        <button class="primary" onclick="saveAllConfig()">保存全部配置并立即生效</button>
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
      <div class="card-title">更新源与私有仓库访问授权（可选）</div>
      <div class="muted">
        说明：若 GitHub 仓库设为 <strong>Public（公开）</strong>，无需任何 Token 即可直连更新。
        若仓库保持 <strong>Private（私有）</strong>，GitHub 对未授权请求会返回 <strong>404</strong>，此时请填入只读 GitHub PAT（仅需 Contents: Read 权限）或改用局域网 / 公开直链地址。
      </div>
      <div class="field">
        <label><span>更新源清单地址（支持 GitHub Releases / 局域网共享 / Google Drive / version.json）</span></label>
        <input type="text" id="cfgUpdUrl" placeholder="https://github.com/cqcmd/-/releases/latest">
      </div>
      <div class="grid2" style="gap:10px">
        <div class="field">
          <label><span>GitHub Token (PAT，私有仓库选填)</span><span class="muted" id="tokenStateBadge">未设置</span></label>
          <input type="password" id="cfgGhToken" placeholder="输入新的 Token（留空表示保持原配置不变）">
        </div>
        <div class="field">
          <label><span>后台自动检查周期（小时，1–168）</span></label>
          <input type="number" id="cfgUpdInterval" min="1" max="168">
        </div>
      </div>
      <div class="btn-row">
        <button class="primary" onclick="saveUpdateSettings(false)">保存更新源配置</button>
        <button onclick="saveUpdateSettings(true)">清除已保存的 Token</button>
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
    <span id="toastText">本地运维控制台已就绪</span>
    <span class="muted mono" id="connState">已连接 127.0.0.1:3000</span>
  </div>
</div>

<script>
let activeTab = 'overview';
let volumeDebounce = null;
let lastLoadedConfig = null;

function pad2(n){ return String(Math.max(0, Number(n)||0)).padStart(2,'0'); }
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
  if(id === 'settings' || id === 'updates') loadConfigFromServer();
  if(id === 'logs') loadLogs();
}

async function postAction(url, body){
  try{
    const opt = { method:'POST', headers:{'Content-Type':'application/json'} };
    if(body !== undefined) opt.body = JSON.stringify(body);
    const r = await fetch(url, opt);
    const j = await r.json();
    if(j.ok){
      showToast(j.message || '操作成功', false);
    }else{
      showToast(j.error || '操作失败', true);
    }
    setTimeout(refreshStatus, 180);
  }catch(e){
    showToast('请求失败，请确认软件仍在运行', true);
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

function setMinutesPreset(val){
  document.getElementById('cfgMinutes').value = val;
}

function previewSelectedVoice(){
  postAction('/api/action/test-voice', {
    tts_engine_index: Number(document.getElementById('cfgTtsEngine').value),
    edge_voice_index: Number(document.getElementById('cfgEdgeVoice').value),
    tts_rate: Number(document.getElementById('cfgTtsRate').value || 0),
  });
}

async function loadConfigFromServer(){
  try{
    const r = await fetch('/api/config');
    const c = await r.json();
    lastLoadedConfig = c;
    document.getElementById('cfgMinutes').value = c.chime_minutes_str || '0, 30';
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
    document.getElementById('cfgMealEnabled').checked = !!c.meal_enabled;
    document.getElementById('cfgMealTime').value = pad2(c.meal_hour)+':'+pad2(c.meal_minute);
    document.getElementById('cfgMealRepeat').value = c.meal_repeat_count ?? 1;
    document.getElementById('cfgMealChimeBefore').checked = !!c.meal_play_chime_before_custom;
    document.getElementById('cfgMealTtsEnabled').checked = !!c.meal_tts_enabled;
    document.getElementById('cfgMealText').value = c.meal_tts_text || '';
    document.getElementById('cfgMealAudio').value = c.meal_custom_audio_path || '';
    document.getElementById('cfgHolidayEnabled').checked = !!c.holiday_enabled;
    document.getElementById('cfgHolidayTpl').value = c.holiday_greeting_template || '';
    document.getElementById('cfgHolidayRules').value = c.holiday_custom_rules || '';
    document.getElementById('cfgHolidayUrl').value = c.holiday_data_source_url || '';

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
  const [qsH, qsM] = parseTimeStr(document.getElementById('cfgQuietStart').value, 0, 0);
  const [qeH, qeM] = parseTimeStr(document.getElementById('cfgQuietEnd').value, 8, 0);
  const [mH, mM] = parseTimeStr(document.getElementById('cfgMealTime').value, 12, 0);
  const payload = {
    chime_minutes_str: document.getElementById('cfgMinutes').value,
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
    meal_enabled: document.getElementById('cfgMealEnabled').checked,
    meal_hour: mH, meal_minute: mM,
    meal_repeat_count: Number(document.getElementById('cfgMealRepeat').value || 1),
    meal_play_chime_before_custom: document.getElementById('cfgMealChimeBefore').checked,
    meal_tts_enabled: document.getElementById('cfgMealTtsEnabled').checked,
    meal_tts_text: document.getElementById('cfgMealText').value,
    meal_custom_audio_path: document.getElementById('cfgMealAudio').value,
    holiday_enabled: document.getElementById('cfgHolidayEnabled').checked,
    holiday_greeting_template: document.getElementById('cfgHolidayTpl').value,
    holiday_custom_rules: document.getElementById('cfgHolidayRules').value,
    holiday_data_source_url: document.getElementById('cfgHolidayUrl').value,
  };
  await postAction('/api/config', payload);
  await loadConfigFromServer();
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
    document.getElementById('ntpBtn').disabled = !!d.is_syncing;
    const slider = document.getElementById('liveVol');
    if(document.activeElement !== slider && d.volume !== undefined){
      slider.value = d.volume;
      document.getElementById('volLabel').textContent = d.volume + '%';
    }
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
    if(d.holiday_feed_status){
      document.getElementById('cfgHolidayStatus').textContent = '外部源状态：' + d.holiday_feed_status;
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
