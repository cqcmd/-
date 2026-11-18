use chrono::{DateTime, Datelike, Local, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 1900-01-01 至 1970-01-01 之间的秒数差（70 年含 17 个闰年）
const NTP_TO_UNIX_EPOCH_SECS: u64 = 2_208_988_800;

/// 内置国内公共 NTP 容灾池（阿里云 / 腾讯云 / 中科院国家授时中心 / 中国 NTP 池）
pub const DOMESTIC_FALLBACK_NTP_SERVERS: &[&str] = &[
    "ntp.aliyun.com",
    "ntp.tencent.com",
    "ntp.ntsc.ac.cn",
    "cn.ntp.org.cn",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NtpSyncResult {
    pub success: bool,
    pub message: String,
    pub synced_at: String,
    pub offset_ms: i64,
}

/// 解析逗号/中文逗号/分号分隔的 NTP 服务器列表；使用默认国内主源时自动附带国内容灾备选节点
pub fn build_ntp_candidate_servers(raw: &str) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    for token in raw.split([',', '，', ';', '；']) {
        let s = token.trim();
        if !s.is_empty() && !list.iter().any(|existing| existing.eq_ignore_ascii_case(s)) {
            list.push(s.to_string());
        }
    }
    if list.is_empty() || list.iter().any(|s| s.eq_ignore_ascii_case("ntp.aliyun.com")) {
        for &fb in DOMESTIC_FALLBACK_NTP_SERVERS {
            if !list.iter().any(|existing| existing.eq_ignore_ascii_case(fb)) {
                list.push(fb.to_string());
            }
        }
    }
    list
}

/// 纯标准库 UDP SNTP（RFC 4330）请求并计算高精度时钟偏差
pub fn query_ntp_utc(server: &str) -> Result<(DateTime<Utc>, i64), String> {
    let addr_input = if server.contains(':') {
        server.to_string()
    } else {
        format!("{server}:123")
    };

    let mut addrs = addr_input
        .to_socket_addrs()
        .map_err(|e| format!("无法解析 NTP 服务器“{server}”：{e}"))?;
    let target = addrs
        .next()
        .ok_or_else(|| format!("NTP 服务器“{server}”未返回有效地址"))?;

    let bind_addr = if target.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let socket = UdpSocket::bind(bind_addr)
        .map_err(|e| format!("创建 UDP 套接字失败：{e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("设置超时失败：{e}"))?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("设置写超时失败：{e}"))?;

    // 构造 48 字节 SNTP v4 客户端请求包（LI=0, VN=4, Mode=3 -> 0x23）
    let mut packet = [0u8; 48];
    packet[0] = 0x23;

    let t1_sys = SystemTime::now();
    socket
        .send_to(&packet, target)
        .map_err(|e| format!("发送 NTP 请求失败：{e}"))?;

    let mut resp = [0u8; 48];
    let (len, _) = socket
        .recv_from(&mut resp)
        .map_err(|e| format!("接收 NTP 响应超时或失败：{e}"))?;
    let t4_sys = SystemTime::now();

    if len < 48 {
        return Err("NTP 服务器返回数据包长度无效".to_string());
    }

    parse_ntp_transmit_timestamp(&resp, t1_sys, t4_sys)
}

/// 解析响应包中的 Transmit Timestamp（偏移 40..48），并结合往返半程延迟估算当前 UTC
pub fn parse_ntp_transmit_timestamp(
    resp: &[u8; 48],
    t1_sys: SystemTime,
    t4_sys: SystemTime,
) -> Result<(DateTime<Utc>, i64), String> {
    let stratum = resp[1];
    if stratum == 0 {
        return Err("NTP 服务器返回 Kiss-o'-Death（Stratum 0），拒绝同步".to_string());
    }

    let secs_1900 = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]) as u64;
    let frac = u32::from_be_bytes([resp[44], resp[45], resp[46], resp[47]]) as u64;

    if secs_1900 < NTP_TO_UNIX_EPOCH_SECS {
        return Err("NTP 时间戳早于 Unix 纪元，数据异常".to_string());
    }

    let unix_secs = secs_1900 - NTP_TO_UNIX_EPOCH_SECS;
    let nanos = ((frac * 1_000_000_000) >> 32) as u32;

    let rtt = t4_sys
        .duration_since(t1_sys)
        .unwrap_or_else(|_| Duration::from_millis(0));
    let half_rtt_ns = (rtt.as_nanos() / 2) as i64;

    let server_dt = Utc
        .timestamp_opt(unix_secs as i64, nanos)
        .single()
        .ok_or_else(|| "转换 NTP 时间戳失败".to_string())?
        + chrono::Duration::nanoseconds(half_rtt_ns);

    let local_unix_ms = t4_sys
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let server_unix_ms = server_dt.timestamp_millis();
    let offset_ms = server_unix_ms - local_unix_ms;

    Ok((server_dt, offset_ms))
}

/// 按候选列表依次向国内/自定义 NTP 节点取时，返回命中的服务器地址
pub fn query_ntp_utc_with_fallback(server: &str) -> Result<(DateTime<Utc>, i64, String), String> {
    let candidates = build_ntp_candidate_servers(server);
    let mut last_err = String::from("未配置有效的 NTP 服务器");
    for candidate in candidates {
        match query_ntp_utc(&candidate) {
            Ok((utc_dt, offset_ms)) => return Ok((utc_dt, offset_ms, candidate)),
            Err(err) => {
                last_err = err;
            }
        }
    }
    Err(last_err)
}

/// 执行完整校时流程（先直调 SetSystemTime；若权限不足且允许 UAC，则拉起提权子进程完成校时）
pub fn sync_system_time_with_elevation(server: &str) -> NtpSyncResult {
    let (utc_dt, offset_ms, matched_server) = match query_ntp_utc_with_fallback(server) {
        Ok(res) => res,
        Err(err) => {
            return NtpSyncResult {
                success: false,
                message: err,
                synced_at: Local::now().format("%H:%M:%S").to_string(),
                offset_ms: 0,
            };
        }
    };

    #[cfg(target_os = "windows")]
    {
        match apply_windows_system_time(utc_dt) {
            Ok(()) => NtpSyncResult {
                success: true,
                message: format!("校时成功（{matched_server}，校准偏差 {offset_ms:+} ms）"),
                synced_at: Local::now().format("%H:%M:%S").to_string(),
                offset_ms,
            },
            Err(WinTimeError::AccessDenied) => {
                // 日常以普通权限驻留托盘时，按需通过 UAC 提权短生命周期子进程完成系统时钟写入
                match run_elevated_ntp_helper(&matched_server) {
                    Ok(elevated_res) => elevated_res,
                    Err(err) => NtpSyncResult {
                        success: false,
                        message: err,
                        synced_at: Local::now().format("%H:%M:%S").to_string(),
                        offset_ms,
                    },
                }
            }
            Err(WinTimeError::Other(msg)) => NtpSyncResult {
                success: false,
                message: msg,
                synced_at: Local::now().format("%H:%M:%S").to_string(),
                offset_ms,
            },
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = utc_dt;
        NtpSyncResult {
            success: false,
            message: format!(
                "非 Windows 环境仅支持读取 NTP 时间（{matched_server}，偏差 {offset_ms:+} ms）"
            ),
            synced_at: Local::now().format("%H:%M:%S").to_string(),
            offset_ms,
        }
    }
}

/// 提权子进程入口：处理 `--sync-ntp <server>` 参数
pub fn run_elevated_cli_mode(server: &str) -> i32 {
    let result_path = elevated_result_file_path();
    let result = match query_ntp_utc_with_fallback(server) {
        Ok((utc_dt, offset_ms, matched_server)) => {
            #[cfg(target_os = "windows")]
            {
                match apply_windows_system_time(utc_dt) {
                    Ok(()) => NtpSyncResult {
                        success: true,
                        message: format!("校时成功（{matched_server}，校准偏差 {offset_ms:+} ms）"),
                        synced_at: Local::now().format("%H:%M:%S").to_string(),
                        offset_ms,
                    },
                    Err(e) => NtpSyncResult {
                        success: false,
                        message: format!("写入系统时间失败：{e:?}"),
                        synced_at: Local::now().format("%H:%M:%S").to_string(),
                        offset_ms,
                    },
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = utc_dt;
                NtpSyncResult {
                    success: false,
                    message: "仅支持在 Windows 上修改系统时钟".to_string(),
                    synced_at: Local::now().format("%H:%M:%S").to_string(),
                    offset_ms,
                }
            }
        }
        Err(err) => NtpSyncResult {
            success: false,
            message: err,
            synced_at: Local::now().format("%H:%M:%S").to_string(),
            offset_ms: 0,
        },
    };

    if let Ok(json) = serde_json::to_string(&result) {
        let _ = std::fs::write(result_path, json);
    }

    if result.success { 0 } else { 1 }
}

fn elevated_result_file_path() -> std::path::PathBuf {
    std::env::temp_dir().join("half_hour_chime_ntp_sync.json")
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
enum WinTimeError {
    AccessDenied,
    Other(String),
}

#[cfg(target_os = "windows")]
fn apply_windows_system_time(utc_dt: DateTime<Utc>) -> Result<(), WinTimeError> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_PRIVILEGE_NOT_HELD, GetLastError, HANDLE, LUID, SYSTEMTIME};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SYSTEMTIME_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::SystemInformation::SetSystemTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_ok()
        {
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(None, SE_SYSTEMTIME_NAME, &mut luid).is_ok() {
                let mut tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    ..Default::default()
                };
                tp.Privileges[0].Luid = luid;
                tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
                let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
            }
            let _ = CloseHandle(token);
        }

        let st = SYSTEMTIME {
            wYear: utc_dt.year() as u16,
            wMonth: utc_dt.month() as u16,
            wDayOfWeek: utc_dt.weekday().num_days_from_sunday() as u16,
            wDay: utc_dt.day() as u16,
            wHour: utc_dt.hour() as u16,
            wMinute: utc_dt.minute() as u16,
            wSecond: utc_dt.second() as u16,
            wMilliseconds: (utc_dt.nanosecond() / 1_000_000) as u16,
        };

        if SetSystemTime(&st).is_ok() {
            Ok(())
        } else {
            let err = GetLastError();
            if err == ERROR_PRIVILEGE_NOT_HELD || err == ERROR_ACCESS_DENIED {
                Err(WinTimeError::AccessDenied)
            } else {
                Err(WinTimeError::Other(format!(
                    "系统时间写入失败（Win32 错误码：{err:?}）"
                )))
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn run_elevated_ntp_helper(server: &str) -> Result<NtpSyncResult, String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::WaitForSingleObject;
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败：{e}"))?;
    let result_path = elevated_result_file_path();
    let _ = std::fs::remove_file(&result_path);

    let exe_h = HSTRING::from(exe.as_os_str());
    let params_h = HSTRING::from(format!("--sync-ntp \"{server}\""));

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe_h.as_ptr()),
        lpParameters: PCWSTR(params_h.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe {
        if ShellExecuteExW(&mut sei).is_err() {
            return Err("用户取消了管理员授权，无法写入系统时钟".to_string());
        }
        if !sei.hProcess.is_invalid() {
            let wait = WaitForSingleObject(sei.hProcess, 15_000);
            let _ = CloseHandle(sei.hProcess);
            if wait != WAIT_OBJECT_0 {
                return Err("提权校时操作超时".to_string());
            }
        }
    }

    let raw = std::fs::read_to_string(&result_path)
        .map_err(|_| "未获取到校时结果，请确认管理员权限已通过".to_string())?;
    let _ = std::fs::remove_file(&result_path);
    serde_json::from_str::<NtpSyncResult>(&raw)
        .map_err(|e| format!("解析校时结果失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ntp_transmit_timestamp_valid() {
        // 构造一个有效 NTP 响应包：stratum=2，时间 = 2026-11-13T00:00:00Z
        let target_dt = Utc.with_ymd_and_hms(2026, 11, 13, 0, 0, 0).unwrap();
        let ntp_secs = (target_dt.timestamp() as u64 + NTP_TO_UNIX_EPOCH_SECS) as u32;

        let mut resp = [0u8; 48];
        resp[1] = 2; // stratum 2
        resp[40..44].copy_from_slice(&ntp_secs.to_be_bytes());
        resp[44..48].copy_from_slice(&0u32.to_be_bytes());

        let t1 = UNIX_EPOCH + Duration::from_secs(target_dt.timestamp() as u64);
        let t4 = t1 + Duration::from_millis(20);
        let (parsed, _) = parse_ntp_transmit_timestamp(&resp, t1, t4).unwrap();
        assert_eq!(parsed.timestamp(), target_dt.timestamp());
    }

    #[test]
    fn test_parse_ntp_stratum_zero_rejected() {
        let resp = [0u8; 48];
        let now = SystemTime::now();
        assert!(parse_ntp_transmit_timestamp(&resp, now, now).is_err());
    }

    #[test]
    fn test_build_domestic_ntp_candidate_servers() {
        let default_list = build_ntp_candidate_servers("ntp.aliyun.com");
        assert_eq!(default_list[0], "ntp.aliyun.com");
        assert!(default_list.contains(&"ntp.tencent.com".to_string()));
        assert!(default_list.contains(&"ntp.ntsc.ac.cn".to_string()));
        assert!(default_list.contains(&"cn.ntp.org.cn".to_string()));

        let custom_lan = build_ntp_candidate_servers("192.168.1.1");
        assert_eq!(custom_lan, vec!["192.168.1.1".to_string()]);

        let multi = build_ntp_candidate_servers("192.168.1.1, ntp.aliyun.com; 192.168.1.1");
        assert_eq!(multi[0], "192.168.1.1");
        assert_eq!(multi[1], "ntp.aliyun.com");
        assert!(multi.contains(&"ntp.ntsc.ac.cn".to_string()));
    }
}
