//! Startup navigation, readiness signals, and content-free phase timing.
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const FRONTEND_READY_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAINTENANCE_GRACE: Duration = Duration::from_secs(3);
static STARTED: OnceLock<Instant> = OnceLock::new();
static FRONTEND_READY: OnceLock<ReadySignal> = OnceLock::new();
static INITIAL_USAGE_READY: OnceLock<ReadySignal> = OnceLock::new();

struct ReadySignal(tokio::sync::watch::Sender<bool>);
impl ReadySignal {
    fn new() -> Self {
        let (sender, _) = tokio::sync::watch::channel(false);
        Self(sender)
    }
    fn mark(&self) -> bool { !self.0.send_replace(true) }
    async fn wait(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            if *receiver.borrow_and_update() { return; }
            if receiver.changed().await.is_err() { return; }
        }
    }
}

pub fn begin() {
    let _ = STARTED.set(Instant::now());
    mark("native.run");
}

pub fn mark(phase: &'static str) {
    let elapsed = STARTED.get().map(|started| started.elapsed().as_millis()).unwrap_or(0);
    eprintln!("[startup-perf] phase={phase} since_start_ms={elapsed}");
}

pub struct PhaseTimer {
    phase: &'static str,
    started: Instant,
}
impl Drop for PhaseTimer {
    fn drop(&mut self) {
        eprintln!("[startup-perf] phase={} elapsed_ms={}", self.phase, self.started.elapsed().as_millis());
    }
}
pub fn phase(name: &'static str) -> PhaseTimer {
    PhaseTimer { phase: name, started: Instant::now() }
}

#[tauri::command]
pub fn startup_frontend_ready(frontend_elapsed_ms: Option<f64>) {
    if FRONTEND_READY.get_or_init(ReadySignal::new).mark() {
        mark("frontend.shell_ready");
        if let Some(elapsed) = frontend_elapsed_ms.filter(|value| value.is_finite() && *value >= 0.0) {
            eprintln!("[startup-perf] phase=frontend.navigation elapsed_ms={elapsed:.1}");
        }
    }
}

pub async fn wait_for_frontend_ready() {
    let ready = FRONTEND_READY.get_or_init(ReadySignal::new);
    if tokio::time::timeout(FRONTEND_READY_TIMEOUT, ready.wait()).await.is_err() {
        // Hidden/tray windows may not produce animation frames. Maintenance must
        // still start even if there is no frontend acknowledgement.
        mark("frontend.ready_timeout");
    }
}

pub fn mark_initial_usage_ready() {
    INITIAL_USAGE_READY.get_or_init(ReadySignal::new).mark();
    mark("maintenance.local_ready");
}

pub async fn wait_for_initial_usage_ready() {
    INITIAL_USAGE_READY.get_or_init(ReadySignal::new).wait().await;
}

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
    #[tokio::test]
    async fn readiness_signal_is_retained_and_idempotent() {
        let signal = ReadySignal::new();
        assert!(signal.mark());
        assert!(!signal.mark());
        assert!(tokio::time::timeout(Duration::from_secs(1), signal.wait()).await.is_ok());
    }

    #[tokio::test]
    async fn readiness_signal_releases_all_waiters() {
        let signal = std::sync::Arc::new(ReadySignal::new());
        let mut waiting = Vec::new();
        for _ in 0..2 {
            let signal = signal.clone();
            waiting.push(tokio::spawn(async move { signal.wait().await }));
        }
        tokio::task::yield_now().await;
        signal.mark();
        for waiter in waiting {
            tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
        }
    }

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
