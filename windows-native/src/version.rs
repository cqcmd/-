use serde::Deserialize;
use std::cmp::Ordering;

/// 版本与发行方元数据（出品方：Z.W.&AI；版本号规则：3段式+日期，每段满10进位，每次编译自动 +1）
pub const APP_VENDOR: &str = "Z.W.&AI";
pub const APP_PKG_VERSION: &str = match option_env!("APP_BUILD_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// 三段式 + 日期版本号模型（本地构建每段 0–9 满 10 进位；远端对比兼容无日期或更大数字段）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreePartDateVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub date: String,
}

impl Ord for ThreePartDateVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => {
                // 若远端未提供日期后缀而本地有日期后缀，同 X.Y.Z 下不视为更新
                self.date.cmp(&other.date)
            }
            ord => ord,
        }
    }
}

impl PartialOrd for ThreePartDateVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ThreePartDateVersion {
    /// 解析形如 "2.0.0+20261114"、"v2.0.1"、"2.0.1-20261114" 的版本号
    pub fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim().trim_start_matches('v').trim_start_matches('V');
        if trimmed.is_empty() {
            return Err("版本号不能为空".to_string());
        }

        let (core, raw_date) = if let Some((c, d)) = trimmed.split_once('+') {
            (c, Some(d))
        } else if let Some((c, d)) = trimmed.split_once('-') {
            (c, Some(d))
        } else {
            (trimmed, None)
        };

        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return Err(format!("版本号格式无效（应为 X.Y.Z 或 X.Y.Z+YYYYMMDD）：{input}"));
        }

        let major: u32 = parts[0]
            .trim()
            .parse()
            .map_err(|_| format!("主版本号第1段不是有效整数：{input}"))?;
        let minor: u32 = parts[1]
            .trim()
            .parse()
            .map_err(|_| format!("主版本号第2段不是有效整数：{input}"))?;
        let patch: u32 = parts[2]
            .trim()
            .parse()
            .map_err(|_| format!("主版本号第3段不是有效整数：{input}"))?;

        let date = if let Some(d) = raw_date {
            let digits: String = d.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() == 8 {
                digits
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        Ok(Self {
            major,
            minor,
            patch,
            date,
        })
    }

    /// 递增修订号（每段满10向高段进位，并更新构建日期）
    pub fn bump_patch(&mut self, new_date: &str) {
        self.patch += 1;
        if self.patch >= 10 {
            self.patch = 0;
            self.minor += 1;
            if self.minor >= 10 {
                self.minor = 0;
                self.major += 1;
            }
        }
        self.date = new_date.to_string();
    }

    pub fn to_semver_string(&self) -> String {
        if self.date.is_empty() {
            format!("{}.{}.{}", self.major, self.minor, self.patch)
        } else {
            format!("{}.{}.{}+{}", self.major, self.minor, self.patch, self.date)
        }
    }
}

/// 服务端版本清单 JSON 结构（标准 version.json）
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RemoteVersionManifest {
    pub version: String,
    #[serde(alias = "downloadUrl")]
    pub download_url: String,
    #[serde(default, alias = "releaseNotes")]
    pub notes: String,
    /// 更新包类型："portable"（默认，单文件原地替换并重启）或 "installer"（启动安装程序）
    #[serde(default, alias = "packageType")]
    pub package_type: String,
}

#[derive(Debug, Deserialize)]
struct GitReleaseAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GitReleaseResponse {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<GitReleaseAsset>,
}

/// 识别并标准化更新检查地址：
/// - 支持 `https://github.com/owner/repo/releases/latest` 自动转为 GitHub Releases API
/// - 支持 `https://drive.google.com/file/d/<id>/view` 自动转为直链下载
pub fn normalize_update_check_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some(drive_id) = extract_google_drive_file_id(trimmed) {
        return format!("https://drive.google.com/uc?export=download&id={drive_id}");
    }

    if let Some((owner, repo)) = extract_github_owner_repo(trimmed) {
        return format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    }

    trimmed.to_string()
}

/// 标准化更新包下载地址（如 Google Drive 分享链接自动转直链确认地址）
pub fn normalize_download_url(input: &str) -> String {
    let trimmed = input.trim();
    if let Some(drive_id) = extract_google_drive_file_id(trimmed) {
        return format!(
            "https://drive.usercontent.google.com/download?id={drive_id}&export=download&confirm=t"
        );
    }
    trimmed.to_string()
}

fn extract_google_drive_file_id(url: &str) -> Option<String> {
    if !url.contains("drive.google.com") && !url.contains("drive.usercontent.google.com") {
        return None;
    }

    if let Some(pos) = url.find("/file/d/") {
        let rest = &url[pos + "/file/d/".len()..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }

    if let Some(q_pos) = url.find('?') {
        for pair in url[q_pos + 1..].split('&') {
            if let Some(val) = pair.strip_prefix("id=") {
                let id: String = val
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }

    None
}

fn extract_github_owner_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let owner = parts[0];
    let repo = parts[1].trim_end_matches(".git");
    if parts.len() == 2 || parts[2].eq_ignore_ascii_case("releases") {
        return Some((owner.to_string(), repo.to_string()));
    }
    None
}

/// 解析更新清单内容：兼容标准 version.json 及 GitHub/Gitee Releases API JSON
pub fn parse_remote_manifest_body(raw_body: &str) -> Result<RemoteVersionManifest, String> {
    let clean = raw_body.trim_start_matches('\u{feff}').trim();
    if clean.is_empty() {
        return Err("更新服务器返回内容为空".to_string());
    }
    let lower = clean.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return Err(
            "更新地址返回了网页内容而非 JSON 清单（若使用 Google Drive 或网盘，请确认文件已设为公开可读且为直链）"
                .to_string(),
        );
    }

    if let Ok(mut manifest) = serde_json::from_str::<RemoteVersionManifest>(clean) {
        manifest.version = manifest.version.trim().to_string();
        manifest.download_url = normalize_download_url(&manifest.download_url);
        return Ok(manifest);
    }

    if let Ok(rel) = serde_json::from_str::<GitReleaseResponse>(clean) {
        let version = if !rel.tag_name.trim().is_empty() {
            rel.tag_name.trim().to_string()
        } else {
            rel.name.trim().to_string()
        };
        if !version.is_empty() {
            let exe_asset = rel
                .assets
                .iter()
                .find(|a| {
                    let n = a.name.to_ascii_lowercase();
                    n.ends_with(".exe") && n.contains("halfhourchime")
                })
                .or_else(|| {
                    rel.assets
                        .iter()
                        .find(|a| a.name.to_ascii_lowercase().ends_with(".exe"))
                })
                .or_else(|| {
                    rel.assets
                        .iter()
                        .find(|a| a.name.to_ascii_lowercase().ends_with(".msi"))
                })
                .or_else(|| rel.assets.first());

            let download_url = exe_asset
                .map(|a| a.browser_download_url.clone())
                .filter(|u| !u.trim().is_empty())
                .unwrap_or(rel.html_url);

            let package_type = if download_url.to_ascii_lowercase().ends_with(".msi") {
                "installer".to_string()
            } else {
                "portable".to_string()
            };

            return Ok(RemoteVersionManifest {
                version,
                download_url: normalize_download_url(&download_url),
                notes: rel.body,
                package_type,
            });
        }
    }

    serde_json::from_str::<RemoteVersionManifest>(clean)
        .map_err(|e| format!("更新清单 JSON 格式无效（需包含 version 与 download_url，或 GitHub Releases JSON）：{e}"))
}

/// 对比远端版本字符串是否高于当前构建版本
pub fn is_newer_than_current(remote_version: &str) -> Result<bool, String> {
    let local = ThreePartDateVersion::parse(APP_PKG_VERSION)?;
    let remote = ThreePartDateVersion::parse(remote_version)?;
    Ok(remote > local)
}

pub fn display_version() -> String {
    format!("v{APP_PKG_VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base10_rollover_carry() {
        let mut v = ThreePartDateVersion::parse("2.0.8+20261113").unwrap();
        v.bump_patch("20261114");
        assert_eq!(v.to_semver_string(), "2.0.9+20261114");

        // 修订段满 10 进位到第 2 段
        v.bump_patch("20261115");
        assert_eq!(v.to_semver_string(), "2.1.0+20261115");

        // 第 2 段与第 3 段均满 10 连续进位到第 1 段
        let mut v2 = ThreePartDateVersion::parse("2.9.9+20261115").unwrap();
        v2.bump_patch("20261116");
        assert_eq!(v2.to_semver_string(), "3.0.0+20261116");
    }

    #[test]
    fn test_version_ordering_and_comparison() {
        let v_base = ThreePartDateVersion::parse("2.0.0+20261113").unwrap();
        let v_newer_date = ThreePartDateVersion::parse("2.0.0+20261114").unwrap();
        let v_newer_patch = ThreePartDateVersion::parse("2.0.1+20261113").unwrap();
        let v_newer_no_date = ThreePartDateVersion::parse("v2.0.1").unwrap();
        let v_newer_minor = ThreePartDateVersion::parse("2.1.0+20261115").unwrap();

        assert!(v_newer_date > v_base);
        assert!(v_newer_patch > v_newer_date);
        assert!(v_newer_no_date > v_newer_date);
        assert!(v_newer_minor > v_newer_patch);
    }

    #[test]
    fn test_parse_remote_manifest_formats_and_bom() {
        let raw_bom = "\u{feff}{\"version\":\"2.0.1+20261120\",\"download_url\":\"https://drive.google.com/file/d/1AbCdEfGhIjKlMnOp/view?usp=sharing\",\"notes\":\"测试\"}";
        let parsed = parse_remote_manifest_body(raw_bom).unwrap();
        assert_eq!(parsed.version, "2.0.1+20261120");
        assert_eq!(
            parsed.download_url,
            "https://drive.usercontent.google.com/download?id=1AbCdEfGhIjKlMnOp&export=download&confirm=t"
        );
        assert!(is_newer_than_current(&parsed.version).unwrap());

        let github_json = r#"{
            "tag_name": "v2.0.9",
            "body": "GitHub Release notes",
            "html_url": "https://github.com/zwai/half-hour-chime/releases/tag/v2.0.9",
            "assets": [
                {
                    "name": "HalfHourChime.exe",
                    "browser_download_url": "https://github.com/zwai/half-hour-chime/releases/download/v2.0.9/HalfHourChime.exe"
                }
            ]
        }"#;
        let gh = parse_remote_manifest_body(github_json).unwrap();
        assert_eq!(gh.version, "v2.0.9");
        assert!(gh.download_url.ends_with("/HalfHourChime.exe"));
        assert!(is_newer_than_current(&gh.version).unwrap());
    }

    #[test]
    fn test_normalize_github_and_drive_urls() {
        assert_eq!(
            normalize_update_check_url("https://github.com/foo/bar/releases/latest"),
            "https://api.github.com/repos/foo/bar/releases/latest"
        );
        assert_eq!(
            normalize_update_check_url("https://drive.google.com/file/d/XYZ_123/view?usp=sharing"),
            "https://drive.google.com/uc?export=download&id=XYZ_123"
        );
    }
}
