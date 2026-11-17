use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Howard Hinnant civil-from-days 算法：零外部依赖获取当前 UTC+8（北京时间）YYYYMMDD 日期字符串
fn current_yyyymmdd() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + 8 * 3600;
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}{m:02}{d:02}")
}

/// 解析版本号各段（返回 (major, minor, patch)）
fn parse_core_parts(version_str: &str) -> (u32, u8, u8) {
    let trimmed = version_str
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V');
    let core = trimmed.split(['+', '-']).next().unwrap_or("2.0.0");
    let mut parts = core.split('.');

    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(2);
    let minor: u8 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch: u8 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}

/// 每次编译自动递增版本号：严格执行“3段式+日期，每段满10进位”
fn bump_build_version(current: &str, today: &str) -> (String, u32, u8, u8) {
    let (mut major, mut minor, mut patch) = parse_core_parts(current);

    patch += 1;
    if patch >= 10 {
        patch = 0;
        minor += 1;
        if minor >= 10 {
            minor = 0;
            major += 1;
        }
    }

    (format!("{major}.{minor}.{patch}+{today}"), major, minor, patch)
}

fn main() {
    // 1. 声明对 build_version.txt 与覆盖环境变量的依赖
    let version_file = Path::new("build_version.txt");
    println!("cargo:rerun-if-changed=build_version.txt");
    println!("cargo:rerun-if-env-changed=APP_BUILD_VERSION_OVERRIDE");
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=ui/theme.slint");

    let today = current_yyyymmdd();
    let (next_version, major, minor, patch) =
        if let Ok(override_ver) = std::env::var("APP_BUILD_VERSION_OVERRIDE") {
            let trimmed = override_ver.trim().to_string();
            if !trimmed.is_empty() {
                let (maj, min, pat) = parse_core_parts(&trimmed);
                (trimmed, maj, min, pat)
            } else {
                let base_version = fs::read_to_string(version_file)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
                let bumped = bump_build_version(&base_version, &today);
                let _ = fs::write(version_file, format!("{}\n", bumped.0));
                bumped
            }
        } else {
            let base_version = fs::read_to_string(version_file)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
            let bumped = bump_build_version(&base_version, &today);
            let _ = fs::write(version_file, format!("{}\n", bumped.0));
            bumped
        };

    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    let repo = std::env::var("GITHUB_REPOSITORY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.contains('/'))
        .unwrap_or_else(|| "cqcmd/-".to_string());
    let default_update_url = format!("https://github.com/{repo}/releases/latest");
    println!("cargo:rustc-env=DEFAULT_UPDATE_CHECK_URL={default_update_url}");
    println!("cargo:rustc-env=APP_BUILD_VERSION={next_version}");

    // 2. 编译 Slint 原生 UI 资源
    slint_build::compile("ui/app.slint").expect("Failed to compile Slint UI");

    // 3. 仅在 Windows 目标下嵌入高清图标、高 DPI 清单与 Z.W.&AI 自动递增版本资源（含数字 FILEVERSION 与文本版本）
    #[cfg(target_os = "windows")]
    {
        let ver_u64: u64 = ((u64::from(major) & 0xFFFF) << 48)
            | ((u64::from(minor) & 0xFFFF) << 32)
            | ((u64::from(patch) & 0xFFFF) << 16);
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set_manifest_file("app.manifest");
        res.set_version_info(winresource::VersionInfo::FILEVERSION, ver_u64);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, ver_u64);
        res.set("CompanyName", "Z.W.&AI");
        res.set("ProductName", "半整点提醒");
        res.set("FileDescription", "半整点提醒 - 桌面报时与定时提醒工具");
        res.set("ProductVersion", &next_version);
        res.set("FileVersion", &next_version);
        res.set("LegalCopyright", "Copyright (C) 2026 Z.W.&AI");
        if let Err(err) = res.compile() {
            println!("cargo:warning=winresource compile skipped or failed: {err}");
        }
    }
}
