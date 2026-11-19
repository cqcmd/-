use serde::Deserialize;
use std::cmp::Ordering;

/// 版本与发行方元数据（出品方：Z.W.&AI；版本号规则：3段式+日期，每段满10进位，每次编译自动 +1）
pub const APP_VENDOR: &str = "Z.W.&AI";
pub const APP_PKG_VERSION: &str = match option_env!("APP_BUILD_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
pub const DEFAULT_GITEE_RELEASE_URL: &str = "https://gitee.com/djjz/HalfHourChime/releases/latest";
pub const DEFAULT_GITEE_API_LATEST_URL: &str =
    "https://gitee.com/api/v5/repos/djjz/HalfHourChime/releases/latest";
pub const DEFAULT_UPDATE_URL: &str = match option_env!("DEFAULT_UPDATE_CHECK_URL") {
    Some(v) => v,
    None => DEFAULT_GITEE_RELEASE_URL,
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
    /// GitHub Releases API 资产接口地址（私有仓库需带 Token 通过此地址下载）
    #[serde(default, alias = "apiDownloadUrl")]
    pub api_download_url: String,
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
    url: String,
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
    draft: bool,
    #[serde(default)]
    assets: Vec<GitReleaseAsset>,
}

/// 自动将空地址、占位地址或官方旧 GitHub 仓库地址迁移为当前默认更新源（Gitee 国内直连源）
pub fn migrate_legacy_update_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.contains("updates.zwai.local") {
        return DEFAULT_UPDATE_URL.to_string();
    }
    if is_official_github_release_url(trimmed) {
        return DEFAULT_UPDATE_URL.to_string();
    }
    trimmed.to_string()
}

/// 判断更新地址是否指向本项目官方 GitHub 仓库（用于将旧版默认配置无感迁移至 Gitee 国内源）
pub fn is_official_github_release_url(url: &str) -> bool {
    extract_github_owner_repo(url).is_some_and(|(owner, repo)| {
        owner.eq_ignore_ascii_case("cqcmd")
            && (repo.eq_ignore_ascii_case("HalfHourChime") || repo == "-")
    })
}

/// 判断更新地址是否指向本项目官方 Gitee 仓库
pub fn is_official_gitee_release_url(url: &str) -> bool {
    extract_gitee_owner_repo(url).is_some_and(|(owner, repo)| {
        owner.eq_ignore_ascii_case("djjz") && repo.eq_ignore_ascii_case("HalfHourChime")
    })
}

fn normalize_release_tag(tag_or_version: &str) -> Option<String> {
    let raw = tag_or_version.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw.replace('+', "-");
    let tag = if normalized.starts_with('v') || normalized.starts_with('V') {
        format!("v{}", &normalized[1..])
    } else {
        format!("v{normalized}")
    };
    Some(tag)
}

/// 根据版本号或 Tag 构造 Gitee 官方国内源对应版本的安装包直链
pub fn build_gitee_mirror_exe_url(tag_or_version: &str) -> Option<String> {
    let tag = normalize_release_tag(tag_or_version)?;
    Some(format!(
        "https://gitee.com/djjz/HalfHourChime/releases/download/{tag}/HalfHourChime.exe"
    ))
}

/// 识别并标准化更新检查地址：
/// - 支持 `https://gitee.com/owner/repo/releases/latest` 自动转为 Gitee OpenAPI v5 Releases
/// - 支持自定义 `https://github.com/owner/repo/releases/latest` 转为 API 地址
/// - 支持标准 HTTP(S) `version.json` 地址
pub fn normalize_update_check_url(input: &str) -> String {
    let migrated = migrate_legacy_update_url(input);
    let trimmed = migrated.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some((owner, repo)) = extract_gitee_owner_repo(trimmed) {
        return format!("https://gitee.com/api/v5/repos/{owner}/{repo}/releases/latest");
    }

    if let Some((owner, repo)) = extract_github_owner_repo(trimmed) {
        return format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    }

    trimmed.to_string()
}

/// 标准化更新包下载地址
pub fn normalize_download_url(input: &str) -> String {
    input.trim().to_string()
}

fn is_http_url(target: &str) -> bool {
    let t = target.trim();
    t.starts_with("https://") || t.starts_with("http://")
}

/// 将清单内的 `download_url` 基于 HTTP(S) 清单来源解析为完整下载链接
pub fn resolve_manifest_download_url(manifest_source: &str, raw_download_url: &str) -> String {
    let dl = normalize_download_url(raw_download_url);
    if dl.is_empty() || is_http_url(&dl) {
        return dl;
    }

    let rel = dl.trim_start_matches("./").trim_start_matches('/');
    let src = manifest_source.trim();
    if is_http_url(src) {
        let base = if let Some(pos) = src.rfind('/') {
            &src[..=pos]
        } else {
            src
        };
        return format!("{}/{}", base.trim_end_matches('/'), rel);
    }

    dl
}

pub fn extract_github_owner_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim();
    if let Some(rest) = trimmed
        .strip_prefix("https://api.github.com/repos/")
        .or_else(|| trimmed.strip_prefix("http://api.github.com/repos/"))
    {
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    let rest = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))?;
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

pub fn extract_gitee_owner_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim();
    if let Some(rest) = trimmed
        .strip_prefix("https://gitee.com/api/v5/repos/")
        .or_else(|| trimmed.strip_prefix("http://gitee.com/api/v5/repos/"))
    {
        let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }

    let rest = trimmed
        .strip_prefix("https://gitee.com/")
        .or_else(|| trimmed.strip_prefix("http://gitee.com/"))?;
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

fn asset_effective_name_lower(asset: &GitReleaseAsset) -> String {
    let trimmed_name = asset.name.trim();
    if !trimmed_name.is_empty() {
        return trimmed_name.to_ascii_lowercase();
    }
    asset
        .browser_download_url
        .split('?')
        .next()
        .and_then(|p| p.rsplit('/').next())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn convert_git_release(rel: GitReleaseResponse) -> Option<RemoteVersionManifest> {
    let version = if !rel.tag_name.trim().is_empty() {
        rel.tag_name.trim().to_string()
    } else {
        rel.name.trim().to_string()
    };
    if version.is_empty() {
        return None;
    }

    let exe_asset = rel
        .assets
        .iter()
        .find(|a| {
            let n = asset_effective_name_lower(a);
            n.ends_with(".exe") && n.contains("halfhourchime")
        })
        .or_else(|| {
            rel.assets
                .iter()
                .find(|a| asset_effective_name_lower(a).ends_with(".exe"))
        })
        .or_else(|| {
            rel.assets
                .iter()
                .find(|a| asset_effective_name_lower(a).ends_with(".msi"))
        })
        .or_else(|| rel.assets.first());

    let download_url = exe_asset
        .map(|a| a.browser_download_url.clone())
        .filter(|u| !u.trim().is_empty())
        .unwrap_or(rel.html_url);

    let api_download_url = exe_asset
        .map(|a| a.url.trim().to_string())
        .unwrap_or_default();

    let package_type = if download_url.to_ascii_lowercase().ends_with(".msi") {
        "installer".to_string()
    } else {
        "portable".to_string()
    };

    Some(RemoteVersionManifest {
        version,
        download_url: normalize_download_url(&download_url),
        api_download_url,
        notes: rel.body,
        package_type,
    })
}

/// 解析更新清单内容：兼容标准 version.json 及 GitHub/Gitee Releases API JSON（含列表数组）
pub fn parse_remote_manifest_body(raw_body: &str) -> Result<RemoteVersionManifest, String> {
    let clean = raw_body.trim_start_matches('\u{feff}').trim();
    if clean.is_empty() {
        return Err("更新服务器返回内容为空".to_string());
    }
    let lower = clean.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return Err(
            "更新地址返回了网页内容而非 JSON 清单，请确认填写的是 Gitee Releases 或 version.json 地址"
                .to_string(),
        );
    }

    if let Ok(mut manifest) = serde_json::from_str::<RemoteVersionManifest>(clean) {
        manifest.version = manifest.version.trim().to_string();
        manifest.download_url = normalize_download_url(&manifest.download_url);
        manifest.api_download_url = manifest.api_download_url.trim().to_string();
        return Ok(manifest);
    }

    if let Ok(rel) = serde_json::from_str::<GitReleaseResponse>(clean) {
        if let Some(manifest) = convert_git_release(rel) {
            return Ok(manifest);
        }
    }

    if let Ok(list) = serde_json::from_str::<Vec<GitReleaseResponse>>(clean) {
        if let Some(rel) = list
            .into_iter()
            .find(|r| !r.draft && (!r.tag_name.trim().is_empty() || !r.name.trim().is_empty()))
        {
            if let Some(manifest) = convert_git_release(rel) {
                return Ok(manifest);
            }
        }
        return Err("更新源仓库暂无已发布的正式 Release 版本".to_string());
    }

    serde_json::from_str::<RemoteVersionManifest>(clean)
        .map_err(|e| format!("更新清单 JSON 格式无效（需包含 version 与 download_url，或 Releases JSON）：{e}"))
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
        let raw_bom = "\u{feff}{\"version\":\"2.0.1+20261120\",\"download_url\":\"http://192.168.1.10/updates/HalfHourChime.exe\",\"notes\":\"测试\"}";
        let parsed = parse_remote_manifest_body(raw_bom).unwrap();
        assert_eq!(parsed.version, "2.0.1+20261120");
        assert_eq!(
            parsed.download_url,
            "http://192.168.1.10/updates/HalfHourChime.exe"
        );
        let parsed_ver = ThreePartDateVersion::parse(&parsed.version).unwrap();
        let baseline_ver = ThreePartDateVersion::parse("2.0.0+20261113").unwrap();
        assert!(parsed_ver > baseline_ver);

        let mut next_ver = ThreePartDateVersion::parse(APP_PKG_VERSION).unwrap();
        next_ver.bump_patch("20991231");
        let next_ver_str = next_ver.to_semver_string();
        assert!(is_newer_than_current(&next_ver_str).unwrap());
        assert!(!is_newer_than_current(APP_PKG_VERSION).unwrap());

        let gitee_tag = format!("v{next_ver_str}");
        let gitee_json = format!(
            r#"{{
            "tag_name": "{gitee_tag}",
            "body": "Gitee Release notes",
            "html_url": "https://gitee.com/djjz/HalfHourChime/releases/tag/{gitee_tag}",
            "assets": [
                {{
                    "name": "HalfHourChime.exe",
                    "browser_download_url": "https://gitee.com/djjz/HalfHourChime/releases/download/{gitee_tag}/HalfHourChime.exe"
                }}
            ]
        }}"#
        );
        let gt = parse_remote_manifest_body(&gitee_json).unwrap();
        assert_eq!(gt.version, gitee_tag);
        assert!(gt.download_url.ends_with("/HalfHourChime.exe"));
        assert!(is_newer_than_current(&gt.version).unwrap());

        let array_json = format!("[{{\"draft\":true,\"tag_name\":\"v9.9.9\"}},{gitee_json}]");
        let gt_arr = parse_remote_manifest_body(&array_json).unwrap();
        assert_eq!(gt_arr.version, gitee_tag);
    }

    #[test]
    fn test_normalize_domestic_gitee_and_http_relative_urls() {
        assert_eq!(
            normalize_update_check_url("https://github.com/cqcmd/HalfHourChime/releases/latest"),
            "https://gitee.com/api/v5/repos/djjz/HalfHourChime/releases/latest"
        );
        assert_eq!(
            normalize_update_check_url("https://github.com/cqcmd/-/releases/latest"),
            "https://gitee.com/api/v5/repos/djjz/HalfHourChime/releases/latest"
        );
        assert_eq!(
            normalize_update_check_url("https://gitee.com/djjz/HalfHourChime/releases/latest"),
            "https://gitee.com/api/v5/repos/djjz/HalfHourChime/releases/latest"
        );
        assert_eq!(
            extract_gitee_owner_repo("https://gitee.com/djjz/HalfHourChime"),
            Some(("djjz".to_string(), "HalfHourChime".to_string()))
        );
        assert!(is_official_gitee_release_url(
            "https://gitee.com/djjz/HalfHourChime/releases/latest"
        ));
        assert!(is_official_github_release_url(
            "https://github.com/cqcmd/HalfHourChime/releases/latest"
        ));
        assert_eq!(
            build_gitee_mirror_exe_url("v2.0.5+20261117").as_deref(),
            Some("https://gitee.com/djjz/HalfHourChime/releases/download/v2.0.5-20261117/HalfHourChime.exe")
        );
        assert_eq!(
            resolve_manifest_download_url("https://example.com/chime/version.json", "HalfHourChime.exe"),
            "https://example.com/chime/HalfHourChime.exe"
        );
    }
}
