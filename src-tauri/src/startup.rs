//! Startup navigation and language only. Removed process-control flags are ignored.
#[derive(Clone, Default, serde::Serialize)]
pub struct StartupOptions {
    pub page: Option<String>,
    pub lang: Option<String>,
}

pub fn parse_options<I, S>(args: I) -> StartupOptions
where I: IntoIterator<Item = S>, S: AsRef<str> {
    let mut options = StartupOptions::default();
    const PAGES: &[&str] = &["memory", "memory-capture", "memory-injection", "memory-help", "pending-memories", "organized-conversations", "skills", "published-skills", "skill-marketplace", "mcp-library", "mcp-tools", "mcp-calls", "mcp-errors", "mcp-help", "usage", "ports", "network", "env-vars", "settings"];
    for arg in args.into_iter().skip(1) {
        let arg = arg.as_ref();
        if let Some(page) = arg.strip_prefix("--page=").filter(|page| PAGES.contains(page)) {
            options.page = Some(page.to_string());
        } else if let Some(lang) = arg.strip_prefix("--lang=").filter(|lang| matches!(*lang, "zh" | "en")) {
            options.lang = Some(lang.to_string());
        }
    }
    options
}

#[tauri::command]
pub fn get_startup_options(state: tauri::State<'_, StartupOptions>) -> StartupOptions {
    state.inner().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_core_pages_and_language() {
        let options = parse_options(["app.exe", "--page=skills", "--lang=en"]);
        assert_eq!(options.page.as_deref(), Some("skills"));
        assert_eq!(options.lang.as_deref(), Some("en"));
    }
    #[test]
    fn removed_runtime_flags_and_routes_are_ignored() {
        for page in ["agents", "configured-agents", "collaboration", "proxy"] {
            let options = parse_options(["app.exe", &format!("--page={page}"), "--select=old", "--start=old", "--open-ui=old"]);
            assert!(options.page.is_none());
        }
    }
}
