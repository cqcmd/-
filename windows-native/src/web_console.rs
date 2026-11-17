use serde::Serialize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
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
    pub edge_tts_voice: String,
    pub volume: u8,
    pub ntp_status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAction {
    TestChime,
    ToggleMute,
    SyncNtp,
}

pub struct WebConsoleController {
    running: Arc<AtomicBool>,
    snapshot: Arc<Mutex<WebSnapshot>>,
    action_tx: Sender<WebAction>,
    active_url: String,
    handle: Option<JoinHandle<()>>,
}

impl WebConsoleController {
    pub fn new(action_tx: Sender<WebAction>) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(Mutex::new(WebSnapshot::default())),
            action_tx,
            active_url: DEFAULT_WEB_CONSOLE_URL.to_string(),
            handle: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn url(&self) -> &str {
        &self.active_url
    }

    pub fn update_snapshot(&self, snap: WebSnapshot) {
        if let Ok(mut guard) = self.snapshot.lock() {
            *guard = snap;
        }
    }

    pub fn start(&mut self, port: u16) -> Result<String, String> {
        if self.is_running() {
            return Ok(self.active_url.clone());
        }
        // 清理可能残留的已结束线程句柄
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
        let action_tx = self.action_tx.clone();

        let join_handle = thread::Builder::new()
            .name("web-console-http".to_string())
            .spawn(move || {
                while running_flag.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                            handle_http_client(stream, &snap_ref, &action_tx);
                        }
                        Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(80));
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

fn handle_http_client(
    mut stream: TcpStream,
    snap_ref: &Arc<Mutex<WebSnapshot>>,
    action_tx: &Sender<WebAction>,
) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req_text = String::from_utf8_lossy(&buf[..n]);
    let first_line = req_text.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");

    match (method, path) {
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
        ("POST", "/api/action/test-chime") => {
            let _ = action_tx.send(WebAction::TestChime);
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                r#"{"ok":true,"message":"已触发测试报时"}"#.as_bytes(),
            );
        }
        ("POST", "/api/action/toggle-mute") => {
            let _ = action_tx.send(WebAction::ToggleMute);
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                r#"{"ok":true,"message":"已切换全局静音状态"}"#.as_bytes(),
            );
        }
        ("POST", "/api/action/sync-ntp") => {
            let _ = action_tx.send(WebAction::SyncNtp);
            write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                r#"{"ok":true,"message":"已发起网络校时"}"#.as_bytes(),
            );
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
<title>半整点提醒 · 本地控制台</title>
<style>
  :root{--bg:#FBF8F3;--card:#FFFFFF;--border:#E6DED1;--text:#262019;--muted:#756A5C;--accent:#B86B21;--accent-fg:#FFFFFF;--ok:#1F7A4C;--warn:#B86B21}
  *{box-sizing:border-box}body{margin:0;padding:24px;font-family:"Microsoft YaHei UI",system-ui,sans-serif;background:var(--bg);color:var(--text)}
  .wrap{max-width:680px;margin:0 auto;display:grid;gap:14px}
  .head{display:flex;justify-content:space-between;align-items:center}
  h1{margin:0;font-size:20px} .ver{font-family:Consolas,monospace;color:var(--accent);font-size:13px;font-weight:700}
  .card{background:var(--card);border:1px solid var(--border);border-radius:10px;padding:18px;display:grid;gap:10px}
  .clock{font-family:Consolas,monospace;font-size:40px;font-weight:700;letter-spacing:1px}
  .row{display:flex;justify-content:space-between;align-items:center;gap:12px;font-size:14px}
  .muted{color:var(--muted);font-size:13px}
  .actions{display:flex;flex-wrap:wrap;gap:10px}
  button{border:1px solid var(--border);background:var(--card);color:var(--text);padding:9px 14px;border-radius:8px;font-size:13px;font-weight:600;cursor:pointer}
  button.primary{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
  button:hover{opacity:.9}
  .toast{min-height:20px;font-size:13px;color:var(--ok)}
</style>
</head>
<body>
<div class="wrap">
  <div class="head">
    <h1>半整点提醒 · 本地控制台</h1>
    <span class="ver" id="ver">--</span>
  </div>
  <div class="card">
    <div class="row">
      <div>
        <div class="clock" id="clock">--:--:--</div>
        <div class="muted" id="date">--</div>
      </div>
      <div style="text-align:right">
        <div id="state" style="font-weight:700">--</div>
        <div class="muted" id="quiet">--</div>
      </div>
    </div>
    <hr style="border:none;border-top:1px solid var(--border);margin:4px 0">
    <div class="row">
      <span class="muted">下一提醒事件：<strong id="nextTitle" style="color:var(--text)">--</strong> (<span id="nextTime">--</span>)</span>
      <span style="font-family:Consolas,monospace;font-weight:700;color:var(--accent)" id="countdown">--</span>
    </div>
  </div>
  <div class="card">
    <div class="row"><span class="muted">报时分钟</span><span id="minutes">--</span></div>
    <div class="row"><span class="muted">语音引擎与音色</span><span id="tts">--</span></div>
    <div class="row"><span class="muted">全局音量</span><span id="vol">--</span></div>
    <div class="row"><span class="muted">校时状态</span><span id="ntp">--</span></div>
  </div>
  <div class="card">
    <div class="actions">
      <button class="primary" onclick="act('/api/action/test-chime')">立即测试报时</button>
      <button onclick="act('/api/action/toggle-mute')" id="muteBtn">切换静音模式</button>
      <button onclick="act('/api/action/sync-ntp')">立即 NTP 校时</button>
    </div>
    <div class="toast" id="toast"></div>
  </div>
</div>
<script>
async function refresh(){
  try{
    const r = await fetch('/api/status');
    const d = await r.json();
    document.getElementById('ver').textContent = d.version || '--';
    document.getElementById('clock').textContent = d.current_time || '--';
    document.getElementById('date').textContent = d.current_date || '--';
    const st = document.getElementById('state');
    st.textContent = d.mute_mode ? '已静音' : (d.is_quiet_now ? '静默时段中' : '正常运行');
    st.style.color = (d.mute_mode || d.is_quiet_now) ? 'var(--warn)' : 'var(--ok)';
    document.getElementById('quiet').textContent = d.quiet_summary || '--';
    document.getElementById('nextTitle').textContent = d.next_event_title || '--';
    document.getElementById('nextTime').textContent = d.next_event_time || '--';
    document.getElementById('countdown').textContent = d.next_event_countdown || '--';
    document.getElementById('minutes').textContent = d.chime_minutes || '--';
    document.getElementById('tts').textContent = d.tts_engine + (d.edge_tts_voice ? ' · ' + d.edge_tts_voice : '');
    document.getElementById('vol').textContent = (d.volume ?? '--') + '%';
    document.getElementById('ntp').textContent = d.ntp_status || '--';
    document.getElementById('muteBtn').textContent = d.mute_mode ? '取消静音模式' : '开启静音模式';
  }catch(e){}
}
async function act(url){
  const t = document.getElementById('toast');
  try{
    const r = await fetch(url, {method:'POST'});
    const j = await r.json();
    t.textContent = j.message || '操作已发送';
    setTimeout(refresh, 200);
  }catch(e){ t.textContent = '请求失败，请检查程序是否仍在运行'; }
}
refresh();
setInterval(refresh, 1000);
</script>
</body>
</html>
"#;
