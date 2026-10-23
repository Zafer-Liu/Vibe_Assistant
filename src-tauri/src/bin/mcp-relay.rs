//! Standalone observed stdio MCP relay binary. Installing it at a hashed path
//! separate from the GUI executable keeps active relays alive when the user
//! exits or terminates the Agent Manager app process.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--mcp-relay") && args.len() == 2 {
        if agent_manager_lib::run_mcp_stdio_relay(&args[1]).is_err() {
            eprintln!("[mcp-relay] unable to start or forward server");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("[mcp-relay] expected --mcp-relay UUID");
    std::process::exit(2);
}
