//! 命令行启动参数（用于脚本化截图、自动化测试等）。
//!
//! 独立成模块而不是直接写在 lib.rs：`#[tauri::command]` 生成的
//! `__cmd__` 宏与同文件 `generate_handler!` 的裸名导入在 crate 根模块
//! 相互冲突（E0255），按 `startup::get_startup_options` 路径注册则无此问题，
//! 也与仓库其余命令的组织方式一致。

/// `--page=<nav-id>` 初始页面；`--lang=<zh|en>` 界面语言；
/// `--select=<agent-id>` 选中指定 Agent（打开详情与日志）；
/// `--start=<agent-id>` 启动指定 Agent；`--open-ui=<agent-id>` 打开其内嵌 UI
///（与 --start 同用时先启动再等待端口就绪后打开）。
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StartupOptions {
    pub page: Option<String>,
    pub lang: Option<String>,
    pub select_agent: Option<String>,
    pub start_agent: Option<String>,
    pub open_ui: Option<String>,
}

/// 解析命令行参数；在 Tauri setup 之前调用一次并 manage 为状态。
pub fn parse_options<I, S>(args: I) -> StartupOptions
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut options = StartupOptions::default();
    for arg in args.into_iter().skip(1) {
        let arg = arg.as_ref();
        if let Some(value) = arg.strip_prefix("--page=") {
            options.page = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--lang=") {
            options.lang = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--select=") {
            options.select_agent = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--start=") {
            options.start_agent = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--open-ui=") {
            options.open_ui = Some(value.to_string());
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
    fn parses_supported_flags_and_ignores_others() {
        let options = parse_options([
            "app.exe",
            "--page=proxy",
            "--lang=en",
            "--select=demo-web",
            "--open-ui=demo-web",
            "--unknown=1",
        ]);
        assert_eq!(options.page.as_deref(), Some("proxy"));
        assert_eq!(options.lang.as_deref(), Some("en"));
        assert_eq!(options.select_agent.as_deref(), Some("demo-web"));
        assert_eq!(options.start_agent, None);
        assert_eq!(options.open_ui.as_deref(), Some("demo-web"));
    }
}
