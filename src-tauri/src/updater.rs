use serde::{Deserialize, Serialize};
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

/// Open a URL with the system default browser.
/// `window.open()` inside the WebView is silently blocked by WebView2, so the
/// frontend calls this command instead.
// shell::open is deprecated in favor of tauri-plugin-opener; we stay on the
// already-registered shell plugin to avoid a new dependency for one call.
// Called from Rust (no scope), it skips JS-side scope validation entirely.
#[allow(deprecated)]
#[tauri::command]
pub async fn open_external_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("Refusing to open non-http(s) URL: {url}"));
    }
    app.shell()
        .open(url, None)
        .map_err(|e| format!("Failed to open URL: {e}"))
}

/// Return just the current app version (useful for the About panel without a network call).
#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}
