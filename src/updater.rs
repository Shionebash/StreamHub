//! Automatic update checker and installer using GitHub Releases.
//!
//! Checks `https://api.github.com/repos/Shionebash/StreamHub/releases/latest`,
//! compares semantic versions (supporting both `x.y` and `x.y.z` tags),
//! downloads the Windows release asset (`.zip` or `.exe`), and swaps the
//! running binary cleanly via a detached helper script.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

pub const REPO_OWNER: &str = "Shionebash";
pub const REPO_NAME: &str = "StreamHub";
pub const RELEASES_PAGE: &str = "https://github.com/Shionebash/StreamHub/releases";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const LATEST_API_URL: &str =
    "https://api.github.com/repos/Shionebash/StreamHub/releases/latest";

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version_label: String,
    pub html_url: String,
    pub notes: String,
    pub asset_name: Option<String>,
    pub download_url: Option<String>,
}

#[derive(Clone, Debug)]
pub enum UpdateCheck {
    UpToDate {
        current: String,
        latest: Option<String>,
    },
    Available(ReleaseInfo),
}

#[derive(Clone, Debug)]
pub struct StagedUpdate {
    pub file_path: PathBuf,
    pub is_zip: bool,
    pub version_label: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

/// Parses `v1.0`, `1.2`, `v1.2.3`, or `1.0.0-rc1` into `(major, minor, patch)`.
pub fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let cleaned = raw
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()?
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    let mut parts = cleaned.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Returns `true` when `remote_tag` represents a strictly newer version than `current`.
pub fn is_newer(remote_tag: &str, current: &str) -> bool {
    match (parse_version(remote_tag), parse_version(current)) {
        (Some(remote), Some(local)) => remote > local,
        _ => false,
    }
}

/// Formats a version for UI display (`1.0.0` -> `v1.0`, `1.2.3` -> `v1.2.3`).
pub fn display_version(raw: &str) -> String {
    match parse_version(raw) {
        Some((major, minor, 0)) => format!("v{major}.{minor}"),
        Some((major, minor, patch)) => format!("v{major}.{minor}.{patch}"),
        None => raw.trim().to_string(),
    }
}

fn select_windows_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    // Prefer a Windows zip package (can include both the exe and libmpv-2.dll).
    if let Some(asset) = assets.iter().find(|a| {
        let lower = a.name.to_ascii_lowercase();
        lower.ends_with(".zip")
            && (lower.contains("win") || lower.contains("x64") || lower.contains("streamhub"))
            && !a.browser_download_url.is_empty()
    }) {
        return Some(asset);
    }
    if let Some(asset) = assets.iter().find(|a| {
        a.name.to_ascii_lowercase().ends_with(".zip") && !a.browser_download_url.is_empty()
    }) {
        return Some(asset);
    }
    assets.iter().find(|a| {
        a.name.to_ascii_lowercase().ends_with(".exe") && !a.browser_download_url.is_empty()
    })
}

/// Queries GitHub Releases for the latest release of `Shionebash/StreamHub`.
pub fn check_latest() -> Result<UpdateCheck> {
    let http = Client::builder()
        .user_agent(format!("StreamHub/{CURRENT_VERSION}"))
        .timeout(Duration::from_secs(12))
        .build()
        .context("could not create HTTP client")?;

    let resp = http
        .get(LATEST_API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("could not reach GitHub Releases")?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(UpdateCheck::UpToDate {
            current: display_version(CURRENT_VERSION),
            latest: None,
        });
    }
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!(
            "GitHub respondió {status}: {}",
            body.chars().take(180).collect::<String>()
        );
    }

    let release: GithubRelease = resp
        .json()
        .context("invalid JSON response from GitHub Releases")?;

    if release.draft || release.prerelease || release.tag_name.trim().is_empty() {
        return Ok(UpdateCheck::UpToDate {
            current: display_version(CURRENT_VERSION),
            latest: None,
        });
    }

    let latest_label = display_version(&release.tag_name);
    if !is_newer(&release.tag_name, CURRENT_VERSION) {
        return Ok(UpdateCheck::UpToDate {
            current: display_version(CURRENT_VERSION),
            latest: Some(latest_label),
        });
    }

    let asset = select_windows_asset(&release.assets);
    let html_url = if release.html_url.trim().is_empty() {
        RELEASES_PAGE.to_string()
    } else {
        release.html_url
    };

    Ok(UpdateCheck::Available(ReleaseInfo {
        tag: release.tag_name,
        version_label: latest_label,
        html_url,
        notes: release.body.trim().to_string(),
        asset_name: asset.map(|a| a.name.clone()),
        download_url: asset.map(|a| a.browser_download_url.clone()),
    }))
}

/// Downloads the release asset into a temporary staging folder.
pub fn download_and_stage(release: &ReleaseInfo) -> Result<StagedUpdate> {
    let url = release
        .download_url
        .as_deref()
        .ok_or_else(|| anyhow!("la release {} no incluye binario para Windows", release.tag))?;
    let name = release
        .asset_name
        .as_deref()
        .unwrap_or("StreamHub-update.exe");

    let stage_dir = std::env::temp_dir().join("StreamHub-update");
    let _ = fs::remove_dir_all(&stage_dir);
    fs::create_dir_all(&stage_dir).context("could not create update staging directory")?;

    let http = Client::builder()
        .user_agent(format!("StreamHub/{CURRENT_VERSION}"))
        .timeout(Duration::from_secs(180))
        .build()
        .context("could not create download HTTP client")?;

    let resp = http
        .get(url)
        .send()
        .with_context(|| format!("failed to download {name}"))?;
    if !resp.status().is_success() {
        bail!("descarga fallida ({})", resp.status());
    }
    let bytes = resp.bytes().context("failed reading update payload")?;
    if bytes.is_empty() {
        bail!("el archivo descargado está vacío");
    }

    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_path = stage_dir.join(&safe_name);
    fs::write(&file_path, &bytes).context("could not save downloaded update")?;

    Ok(StagedUpdate {
        is_zip: safe_name.to_ascii_lowercase().ends_with(".zip"),
        file_path,
        version_label: release.version_label.clone(),
    })
}

/// Spawns a detached helper process that waits for the current instance to exit,
/// replaces the executable (and `libmpv-2.dll` if bundled), and relaunches StreamHub.
pub fn apply_and_restart(staged: &StagedUpdate) -> Result<()> {
    let current_exe = std::env::current_exe().context("could not resolve current executable")?;
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow!("current executable has no parent directory"))?;
    let stage_dir = staged
        .file_path
        .parent()
        .ok_or_else(|| anyhow!("staged file has no parent directory"))?;
    let pid = std::process::id();

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let script_path = stage_dir.join("apply_update.ps1");
        let ps_escape = |p: &std::path::Path| p.to_string_lossy().replace('\'', "''");
        let exe_str = ps_escape(&current_exe);
        let dir_str = ps_escape(exe_dir);
        let file_str = ps_escape(&staged.file_path);
        let stage_str = ps_escape(stage_dir);
        let exe_name = current_exe
            .file_name()
            .map(|n| n.to_string_lossy().replace('\'', "''"))
            .unwrap_or_else(|| "StreamHub.exe".to_string());

        let replace_block = if staged.is_zip {
            format!(
                r#"$extractDir = Join-Path '{stage_str}' 'extracted'
if (Test-Path -LiteralPath $extractDir) {{ Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue }}
Expand-Archive -LiteralPath '{file_str}' -DestinationPath $extractDir -Force
$newExe = Get-ChildItem -LiteralPath $extractDir -Recurse -Filter '*.exe' | Sort-Object {{
    if ($_.Name -ieq '{exe_name}') {{ 0 }}
    elseif ($_.Name -ilike '*streamhub*') {{ 1 }}
    else {{ 2 }}
}} | Select-Object -First 1
if ($null -eq $newExe) {{ throw 'No .exe found in release archive' }}
Copy-Item -LiteralPath $newExe.FullName -Destination '{exe_str}' -Force
$newDll = Get-ChildItem -LiteralPath $extractDir -Recurse -Filter 'libmpv-2.dll' | Select-Object -First 1
if ($null -ne $newDll) {{
    Copy-Item -LiteralPath $newDll.FullName -Destination (Join-Path '{dir_str}' 'libmpv-2.dll') -Force
}}"#
            )
        } else {
            format!("Copy-Item -LiteralPath '{file_str}' -Destination '{exe_str}' -Force")
        };

        let script = format!(
            r#"$ErrorActionPreference = 'Stop'
try {{ Wait-Process -Id {pid} -Timeout 30 -ErrorAction SilentlyContinue }} catch {{}}
Start-Sleep -Milliseconds 450
for ($i = 0; $i -lt 10; $i++) {{
    try {{
        {replace_block}
        break
    }} catch {{
        Start-Sleep -Milliseconds 500
    }}
}}
Start-Process -FilePath '{exe_str}' -WorkingDirectory '{dir_str}'
Start-Sleep -Seconds 2
Remove-Item -LiteralPath '{stage_str}' -Recurse -Force -ErrorAction SilentlyContinue
"#
        );

        fs::write(&script_path, script).context("could not write updater script")?;

        Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-File",
            ])
            .arg(&script_path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("could not launch updater helper")?;

        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = (current_exe, exe_dir, stage_dir, pid);
        bail!("la actualización automática binaria solo está implementada para Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_and_three_part_versions() {
        assert_eq!(parse_version("1.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("v1.0"), Some((1, 0, 0)));
        assert_eq!(parse_version("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v2.0.1-beta"), Some((2, 0, 1)));
    }

    #[test]
    fn compares_versions_correctly() {
        assert!(!is_newer("v1.0", "1.0.0"));
        assert!(is_newer("v1.1", "1.0.0"));
        assert!(is_newer("1.2", "1.1.0"));
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(!is_newer("v0.9", "1.0.0"));
    }

    #[test]
    fn formats_display_versions() {
        assert_eq!(display_version("1.0.0"), "v1.0");
        assert_eq!(display_version("v1.3"), "v1.3");
        assert_eq!(display_version("1.3.2"), "v1.3.2");
    }
}
