mod agent;
mod agent_http;
mod agent_sources;
mod backup;
mod cloud_sync;
#[cfg(test)]
mod test_http;
mod commands;
mod env_manager;
mod github;
mod llm;
mod mcp;
mod mcp_registry;
mod memory_backend;
mod memory_ingest;
mod memory_mcp;
mod mobile_status;
mod network_info;
mod ports;
mod process_util;
mod proxy;
mod pty;
mod skill_registry;
mod skill_marketplace;
mod startup;
mod telemetry_store;
mod thinking;
mod ui_window;
mod updater;
mod vault_remote;
mod worktree;

use agent_http::*;
use commands::*;
use github::*;
use llm::*;
use mcp::*;
use memory_backend::*;
use memory_ingest::*;
use memory_mcp::*;
use ports::*;
use proxy::*;
use pty::*;
use skill_registry::*;
use tauri::{Emitter, Manager};
use telemetry_store::*;
use ui_window::*;
use updater::*;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(startup::parse_options(std::env::args()))
        .manage(agent::AgentStore::new())
        .manage(pty::PtyStore::new())
        .manage(ui_window::UiWebviewStore::new())
        .manage(proxy::TunnelStore::new())
        .manage(memory_backend::MemoryBackend::new())
        .manage(memory_ingest::IngestStore::new())
        .manage(telemetry_store::TelemetryStore::new().expect("initialize telemetry store"))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            // `Child` does not terminate its process on Drop.  If the optional
            // memory backend was explicitly started, stop only the children we
            // own when the main window is destroyed.
            if matches!(event, tauri::WindowEvent::Destroyed) {
                window
                    .app_handle()
                    .state::<memory_backend::MemoryBackend>()
                    .stop();
            }
        })
        .setup(|_app| {
            // 初始化全局 AgentHttpStore 并启动 HTTP server（固定端口，供记忆沉淀/遥测/agent 回调使用）
            let store = agent_http::init_store();
            tauri::async_runtime::spawn(async move {
                agent_http::start_agent_http_server(agent_http::AGENT_HTTP_PORT, store).await;
            });

            // Memory extraction is optional.  Keep event collection local and
            // durable even when its external vector/graph services are absent;
            // starting those services is an explicit user action from the UI.
            let memory = _app
                .state::<memory_backend::MemoryBackend>()
                .inner()
                .clone();
            let shared = memory_backend::init_shared(memory);
            let ingest = _app.state::<memory_ingest::IngestStore>().inner().clone();
            memory_ingest::init_ingest(ingest.clone());
            let telemetry = _app
                .state::<telemetry_store::TelemetryStore>()
                .inner()
                .clone();
            telemetry_store::init_shared(telemetry);
            // Populate local indexes after the window can paint.  These jobs
            // are idempotent and write their result to SQLite, so opening a
            // memory page never needs to re-walk agent directories.
            let startup_handle = _app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let _ = skill_registry::skill_list_impl();
                let _ = memory_ingest::telemetry_backfill_conversations();
                if let Some(store) = telemetry_store::shared_store() {
                    let _ = store.refresh_transcript_usage();
                }
                // The startup rebuild races the frontend's first read; ping
                // the frontend so an open usage page re-queries the ledger.
                let _ = startup_handle.emit("telemetry-updated", ());
            });
            let ingest2 = ingest.clone();
            // 静默会话节流巡检
            memory_ingest::start_idle_flusher(shared, ingest2);
            // L2/L3 后台自动重算：有新 L1 证据且到期时重建，注入文档不再
            // 依赖手动进记忆中心点「整理」。
            memory_ingest::start_l2_l3_refresh_scheduler(_app.handle().clone());
            // 用量账本周期性重扫：常开应用也能持续采集新会话的 Token 用量。
            telemetry_store::start_usage_refresh_scheduler(_app.handle().clone());
            // 云端记忆库定时同步（每分钟检查设置，无需重启）
            cloud_sync::start_cloud_sync_scheduler();
            tauri::async_runtime::spawn(mobile_status::start_mobile_status_server());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            detect_agent_clis,
            import_detected_agent_clis,
            start_agent,
            stop_agent,
            get_agent_logs,
            resolve_agent_cwd,
            save_agent_config,
            delete_agent,
            get_port_status,
            scan_project_dir,
            // MCP
            list_mcp_servers,
            save_mcp_server,
            delete_mcp_server,
            get_mcp_config_path,
            scan_mcp_local,
            parse_mcp_text,
            // Ports
            list_listening_ports,
            kill_port,
            // LLM config
            list_llm_providers,
            save_llm_provider,
            delete_llm_provider,
            test_llm_provider,
            memory_extraction_config_get,
            memory_extraction_config_set,
            ollama_config_get,
            ollama_config_set,
            test_ollama_connection,
            // Agent HTTP spike (phase 4)
            dispatch_agent_task,
            list_agent_tasks,
            list_agent_results,
            // PTY terminal
            pty_start,
            pty_write,
            pty_resize,
            pty_stop,
            pty_resolve_debug,
            // Proxy (Caddy)
            proxy_get_config,
            proxy_save_config,
            proxy_check_caddy,
            proxy_hash_password,
            proxy_apply,
            proxy_stop,
            proxy_status,
            proxy_get_caddyfile,
            proxy_preview_caddyfile,
            // Cloudflare Tunnel
            tunnel_check_cloudflared,
            tunnel_start,
            tunnel_stop,
            tunnel_stop_all,
            tunnel_list,
            tunnel_alive,
            // GitHub install
            github_fetch_repo_info,
            github_clone_repo,
            github_check_git,
            github_get_proxy,
            github_save_token,
            github_token_status,
            // Agent UI window
            open_agent_ui_webview,
            update_agent_ui_webview,
            fullscreen_agent_ui_webview,
            close_agent_ui_webview,
            // Updater
            check_for_update,
            get_app_version,
            // Memory engine
            memory_backend_status,
            memory_backend_start,
            memory_backend_stop,
            memory_backend_request,
            memory_backend::memory_consolidate,
            memory_backend::memory_consolidation_restore,
            memory_backend::memory_importance_refresh,
            memory_backend::memory_importance_list,
            memory_backend::memory_importance_set_pinned,
            // Memory ingest (hook 自动沉淀)
            memory_hook_install,
            memory_hook_uninstall,
            memory_hook_status,
            memory_ingest_status,
            memory_ingest_set_enabled,
            memory_ingest_flush_pending,
            memory_ingest::memory_ingest_organize_conversations,
            memory_ingest::memory_ingest_organize_session,
            memory_ingest::memory_pending_l1_sessions,
            memory_ingest::memory_organized_l1_sessions,
            memory_ingest::memory_l1_conversation_detail,
            memory_ingest::memory_import_folder,
            memory_ingest::local_memory_list,
            memory_ingest::local_memory_stats,
            memory_ingest::local_memory_reset_for_reextraction,
            memory_ingest::local_memory_search,
            memory_ingest::local_memory_update,
            memory_ingest::local_memory_delete,
            memory_ingest::local_memory_add_user,
            memory_ingest::memory_layer_document_update,
            memory_ingest::memory_short_term_consolidate,
            memory_ingest::memory_auto_refresh_get,
            memory_ingest::memory_auto_refresh_set,
            memory_ingest::memory_long_term_profile_draft,
            memory_ingest::memory_long_term_profile_publish,
            memory_ingest::memory_long_term_profile_delete_draft,
            memory_ingest::memory_layer_documents,
            // Agent 数据源（跨设备目录覆盖）
            agent_sources::agent_sources_list,
            agent_sources::agent_source_set_override,
            // Shared memory MCP (Codex / Claude one-click setup)
            memory_mcp_status,
            memory_mcp_install,
            memory_mcp_uninstall,
            claude_desktop_mcp_config_get,
            claude_desktop_mcp_config_set,
            // Usage and event telemetry
            telemetry_summary,
            telemetry_refresh_usage,
            telemetry_live_status,
            telemetry_recent_events,
            telemetry_usage_records,
            telemetry_usage_analytics,
            telemetry_search_conversations,
            memory_mcp_access_logs,
            memory_injection_stats,
            memory_ingest::telemetry_backfill_conversations,
            // Shared Skill registry
            skill_scan,
            skill_list,
            skill_read,
            skill_sync_preview,
            skill_sync_apply,
            skill_set_status,
            skill_set_assignment,
            skill_set_status_bulk,
            skill_set_assignment_bulk,
            skill_publish,
            skill_rollback_latest,
            skill_delete,
            skill_drift_detail,
            skill_published_drift,
            skill_adopt_local,
            skill_sync_apply_one,
            skill_marketplace::skill_marketplace_list,
            skill_marketplace::skill_marketplace_preview,
            skill_marketplace::skill_marketplace_install,
            // MCP registry (MCP 库)
            mcp_registry::mcp_catalog_list,
            mcp_registry::mcp_catalog_migration_report,
            mcp_registry::mcp_catalog_upsert,
            mcp_registry::mcp_catalog_import,
            mcp_registry::mcp_catalog_delete,
            mcp_registry::mcp_equip,
            mcp_registry::mcp_unequip,
            mcp_registry::mcp_status_all,
            mcp_registry::mcp_import_from_agents,
            mcp_registry::mcp_sync_agent,
            mcp_registry::mcp_profile_list,
            mcp_registry::mcp_profile_save,
            mcp_registry::mcp_profile_delete,
            // Backup & restore
            backup::config_export,
            backup::config_import,
            // Cloud Memory Vault
            cloud_sync::cloud_vault_get_settings,
            cloud_sync::cloud_vault_save_settings,
            cloud_sync::cloud_vault_test_connection,
            cloud_sync::cloud_vault_sync,
            cloud_sync::cloud_vault_pull,
            cloud_sync::cloud_vault_status,
            cloud_sync::cloud_vault_list_conflicts,
            cloud_sync::cloud_vault_resolve_conflict,
            mobile_status::mobile_status_get_settings,
            mobile_status::mobile_status_set_enabled,
            mobile_status::mobile_status_rotate_token,
            // Network manager (网卡 / IP / 实时网速)
            network_info::network_interfaces,
            network_info::network_throughput,
            network_info::network_public_ip,
            network_info::network_speed_test,
            // Env manager (系统环境变量)
            env_manager::env_vars_list,
            env_manager::env_var_set,
            env_manager::env_var_delete,
            startup::get_startup_options,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Separate from the Tauri desktop runtime so the same installed executable
/// can act as a stdio MCP server for Codex and Claude.
pub fn run_memory_mcp_stdio() -> Result<(), String> {
    memory_mcp::run_stdio()
}
