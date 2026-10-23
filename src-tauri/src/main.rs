#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|argument| argument == "--mcp-memory") {
        if let Err(error) = agent_manager_lib::run_memory_mcp_stdio() {
            eprintln!("[memory-mcp] {error}");
            std::process::exit(1);
        }
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--mcp-relay") {
        if args.len() != 2 || agent_manager_lib::run_mcp_stdio_relay(&args[1]).is_err() {
            eprintln!("[mcp-relay] unable to start or forward server");
            std::process::exit(1);
        }
        return;
    }
    agent_manager_lib::run();
}
