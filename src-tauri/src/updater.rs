use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use tauri_plugin_shell::ShellExt;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
// Replace with the actual GitHub owner/repo before publishing
const GITHUB_REPO: &str = "Zafer-Liu/Vibe_Assistant";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VersionInfo {
    pub current: String,
    pub latest: String,
    pub has_update: bool,
    pub release_url: String,
    /// Direct download URL of the Windows installer asset (falls back to the release page).
    pub download_url: String,
    pub release_notes: String,
    pub published_at: String,
}

/// Pick the best Windows installer asset from a GitHub release.
/// Preference: NSIS setup.exe > x64 exe > .msi > any exe.
fn pick_windows_installer(assets: &[serde_json::Value]) -> Option<String> {
    let score = |name: &str| -> Option<u8> {
        let lower = name.to_lowercase();
        let is_installer = lower.ends_with(".exe") || lower.ends_with(".msi");
        if !is_installer {
            return None; // skip .blockmap / .zip / signatures
        }
        let mut s = 1u8;
        if lower.contains("setup") {
            s += 4; // NSIS "-setup.exe"
        }
        if lower.contains("x64") || lower.contains("x86_64") {
            s += 2;
        }
        Some(s)
    };
    assets
        .iter()
        .filter_map(|a| {
            let name = a["name"].as_str()?;
            let url = a["browser_download_url"].as_str()?;
            score(name).map(|s| (s, url))
        })
        .max_by_key(|(s, _)| *s)
        .map(|(_, url)| url.to_string())
}

/// Compare two semver strings, returns true if `a` is strictly newer than `b`.
fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let parts: Vec<&str> = s.trim_start_matches('v').split('.').collect();
        let n = |i: usize| parts.get(i).and_then(|p| p.parse().ok()).unwrap_or(0);
        (n(0), n(1), n(2))
    };
    parse(a) > parse(b)
}

/// Fetch the latest release info from GitHub.
/// Uses reqwest which is already declared in Cargo.toml.
#[tauri::command]
pub async fn check_for_update() -> Result<VersionInfo, String> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );

    let client = reqwest::Client::builder()
        .user_agent("agent-manager-desktop")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        return Err(format!("GitHub API returned status {}", status));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let latest_tag = json["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();

    let release_url = json["html_url"].as_str().unwrap_or("").to_string();

    let release_notes = json["body"].as_str().unwrap_or("").to_string();

    let published_at = json["published_at"].as_str().unwrap_or("").to_string();

    let has_update = !latest_tag.is_empty() && is_newer(&latest_tag, CURRENT_VERSION);

    // Prefer a direct installer asset so the browser starts downloading immediately.
    let download_url = json["assets"]
        .as_array()
        .and_then(|assets| pick_windows_installer(assets))
        .unwrap_or_else(|| release_url.clone());

    Ok(VersionInfo {
        current: CURRENT_VERSION.to_string(),
        latest: latest_tag,
        has_update,
        release_url,
        download_url,
        release_notes,
        published_at,
    })
}

fn external_web_url(raw: &str) -> Result<reqwest::Url, String> {
    const INVALID: &str = "Expected an absolute HTTP or HTTPS URL";
    if !(raw.starts_with("https://") || raw.starts_with("http://"))
        || raw.chars().any(|c| c.is_control() || c == '\\')
    {
        return Err(INVALID.into());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| INVALID)?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(INVALID.into());
    }
    Ok(url)
}

/// Hand the web URL to Windows' protocol activation service from the foreground
/// WebView's UI thread. Await acknowledgement without blocking its message loop.
#[cfg(windows)]
async fn launch_web_url(app: tauri::AppHandle, url: reqwest::Url) -> Result<(), String> {
    use windows::{core::HSTRING, Foundation::Uri, System::Launcher};
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let pending = Uri::CreateUri(&HSTRING::from(url.as_str()))
            .and_then(|uri| Launcher::LaunchUriAsync(&uri));
        let _ = tx.send(pending);
    })
    .map_err(|_| "Browser activation could not be scheduled")?;
    let activation_error = |e: windows::core::Error| {
        format!(
            "Windows browser activation failed (0x{:08X})",
            e.code().0 as u32
        )
    };
    let pending = rx
        .await
        .map_err(|_| "Browser activation was interrupted")?
        .map_err(activation_error)?;
    if pending.await.map_err(activation_error)? {
        Ok(())
    } else {
        Err("Windows did not accept the web link; open it in your browser".into())
    }
}

#[cfg(not(windows))]
#[allow(deprecated)]
async fn launch_web_url(app: tauri::AppHandle, url: reqwest::Url) -> Result<(), String> {
    app.shell()
        .open(url.as_str(), None)
        .map_err(|e| format!("Failed to open URL: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WebLinkAction {
    Opened,
    #[cfg(windows)]
    OpenManually,
}

#[tauri::command]
pub async fn open_external_url(
    app: tauri::AppHandle,
    url: String,
) -> Result<WebLinkAction, String> {
    let url = external_web_url(&url)?;
    #[cfg(windows)]
    {
        let scheme = url.scheme().to_owned();
        let safe = tauri::async_runtime::spawn_blocking(move || {
            crate::windows_browser::can_activate(&scheme)
        })
        .await
        .unwrap_or(false);
        if !safe {
            return Ok(WebLinkAction::OpenManually);
        }
    }
    launch_web_url(app, url).await?;
    Ok(WebLinkAction::Opened)
}

#[cfg(test)]
mod tests {
    use super::external_web_url;

    #[test]
    fn browser_activation_accepts_web_links_only() {
        for valid in [
            "https://api.oplist.org/",
            "https://github.com/a/b?x=1&y=2#asset",
            "http://127.0.0.1:8000/",
        ] {
            assert_eq!(external_web_url(valid).unwrap().as_str(), valid);
        }
        for invalid in [
            "file:///C:/",
            "ms-settings:defaultapps",
            "https://",
            "https://user:secret@example.com/",
            "https://example.com/\0",
            "https://example.com/\\path",
        ] {
            assert!(external_web_url(invalid).is_err());
        }
    }
}

/// Return just the current app version (useful for the About panel without a network call).
#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}
