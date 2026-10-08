use serde::{Deserialize, Serialize};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
// Replace with the actual GitHub owner/repo before publishing
const GITHUB_REPO: &str = "Zafer-Liu/Vibe_Assistant";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VersionInfo {
    pub current: String,
    pub latest: String,
    pub has_update: bool,
    pub release_url: String,
    pub release_notes: String,
    pub published_at: String,
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

    Ok(VersionInfo {
        current: CURRENT_VERSION.to_string(),
        latest: latest_tag,
        has_update,
        release_url,
        release_notes,
        published_at,
    })
}

/// Return just the current app version (useful for the About panel without a network call).
#[tauri::command]
pub fn get_app_version() -> String {
    CURRENT_VERSION.to_string()
}
