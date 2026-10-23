"""Exercise relay or missing-relay fallback for Claude Desktop's web-search-prime.

Temporarily registers the existing server in the catalog, changes only its
Claude Desktop node, and restores both. Never prints commands or credentials.
"""

import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parent.parent
CONFIG_ROOT = Path(os.environ["APPDATA"]) / "agent-manager"
CONFIG = Path(os.environ["LOCALAPPDATA"]) / "Claude-3p" / "claude_desktop_config.json"
BACKUP = CONFIG.with_name(CONFIG.name + ".vibe-web-search-smoke-backup")
OBSERVATIONS = CONFIG_ROOT / "mcp-observation"
DATABASE = CONFIG_ROOT / "telemetry.sqlite3"
LAUNCHER = (ROOT / "src-tauri" / "mcp-launcher" / "target" / "debug" / "mcp-launcher.exe").resolve()
RELAY = (ROOT / "src-tauri" / "target" / "debug" / "agent-manager.exe").resolve()
NAME = "web-search-prime"
SETTING = "mcp_catalog"


def atomic_bytes(path, content):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        file.write(content)
        file.flush()
        os.fsync(file.fileno())
    try:
        shutil.copymode(path, temporary)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def json_bytes(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")


def catalog_connection():
    return sqlite3.connect(DATABASE, timeout=10)


def read_catalog(connection):
    row = connection.execute("SELECT value_json FROM app_settings WHERE setting_key = ?", (SETTING,)).fetchone()
    if row is None:
        raise RuntimeError("Central MCP catalog missing")
    return json.loads(row[0])


def update_catalog(connection, entries):
    connection.execute(
        "UPDATE app_settings SET value_json = ?, updated_at = ? WHERE setting_key = ?",
        (json.dumps(entries, ensure_ascii=False, separators=(",", ":")),
         time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), SETTING),
    )


def run_call(assignment, expect_fallback):
    messages = queue.Queue()
    process = subprocess.Popen(
        [str(LAUNCHER), "--mcp-relay", assignment],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )

    def read_stdout():
        for line in process.stdout:
            try:
                messages.put(json.loads(line))
            except (UnicodeError, ValueError):
                pass
        messages.put(None)

    threading.Thread(target=read_stdout, daemon=True).start()

    def response_for(request_id, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                response = messages.get(timeout=min(2, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                if process.poll() is not None:
                    break
                continue
            if response is None:
                break
            if isinstance(response, dict) and response.get("id") == request_id:
                return response
        raise RuntimeError(f"MCP request {request_id} produced no response")

    def send(request):
        process.stdin.write((json.dumps(request, separators=(",", ":")) + "\n").encode("utf-8"))
        process.stdin.flush()

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": {"name": "fallback-smoke", "version": "1.0"}}})
        initialized = response_for(1, 60)
        if "error" in initialized or "result" not in initialized:
            raise RuntimeError("MCP initialization failed")
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        listed = response_for(2, 60)
        tools = listed.get("result", {}).get("tools", [])
        if not isinstance(tools, list) or not any(tool.get("name") == "web_search_prime" for tool in tools):
            raise RuntimeError("web_search_prime was not discovered")
        heartbeat = OBSERVATIONS / "fallback" / assignment / f"{process.pid}.json"
        deadline = time.monotonic() + 5
        while not heartbeat.is_file() and time.monotonic() < deadline:
            time.sleep(0.05)
        if expect_fallback:
            if not heartbeat.is_file():
                raise RuntimeError("Fallback heartbeat was not written")
            state = json.loads(heartbeat.read_text(encoding="utf-8"))
            if state.get("assignment_id") != assignment or state.get("pid") != process.pid:
                raise RuntimeError("Fallback heartbeat did not match the launcher")
        elif heartbeat.exists():
            raise RuntimeError("Healthy relay unexpectedly wrote a fallback heartbeat")
        with sqlite3.connect(f"file:{DATABASE.as_posix()}?mode=ro", uri=True) as database:
            active = database.execute(
                "SELECT COUNT(*) FROM mcp_proxy_connections WHERE assignment_id = ?",
                (hashlib.sha256(assignment.encode()).hexdigest(),),
            ).fetchone()[0]
            before = database.execute(
                "SELECT COUNT(*) FROM mcp_call_observations WHERE source = 'stdio_proxy' AND server_name = ? AND tool_name = ?",
                (NAME, "web_search_prime"),
            ).fetchone()[0]
        if expect_fallback and active:
            raise RuntimeError("Relay connection exists during fallback")
        if not expect_fallback and active < 1:
            raise RuntimeError("Healthy relay did not register an observed connection")
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "web_search_prime", "arguments": {
                "search_query": "Model Context Protocol official documentation",
                "location": "us", "content_size": "medium",
            }}})
        reply = response_for(3, 90)
        result = reply.get("result", {})
        content = result.get("content", []) if isinstance(result, dict) else []
        text = " ".join(item.get("text", "") for item in content if item.get("type") == "text") if isinstance(content, list) else ""
        success = "error" not in reply and isinstance(result, dict) and result.get("isError") is not True and bool(text.strip())
        observed = False
        status = None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            with sqlite3.connect(f"file:{DATABASE.as_posix()}?mode=ro", uri=True) as database:
                rows = database.execute(
                    "SELECT status, result_observed FROM mcp_call_observations "
                    "WHERE source = 'stdio_proxy' AND server_name = ? AND tool_name = ? "
                    "ORDER BY id DESC LIMIT 1 OFFSET ?",
                    (NAME, "web_search_prime", 0),
                ).fetchone()
                count = database.execute(
                    "SELECT COUNT(*) FROM mcp_call_observations "
                    "WHERE source = 'stdio_proxy' AND server_name = ? AND tool_name = ?",
                    (NAME, "web_search_prime"),
                ).fetchone()[0]
            if count > before and rows and rows[0] != "pending":
                status = rows
                observed = True
                break
            if expect_fallback:
                break
            time.sleep(0.05)
        print(json.dumps({"scenario": "fallback" if expect_fallback else "relay", "initialized": True,
                          "tools": len(tools), "relay_connections": active,
                          "fallback_heartbeat": heartbeat.is_file(), "tool_call": "web_search_prime",
                          "tool_call_success": success, "result_chars": len(text) if success else 0,
                          "observed_call": observed, "observed_status": status[0] if status else None,
                          "observed_result": bool(status[1]) if status else False}), flush=True)
        if not success:
            raise RuntimeError("web_search_prime returned a tool error")
        if not expect_fallback and status != ("success", 1):
            raise RuntimeError("Healthy relay did not record a successful tool call")
        if expect_fallback and observed:
            raise RuntimeError("Fallback unexpectedly recorded an observed call")
        process.stdin.close()
        process.wait(timeout=15)
        if process.returncode != 0:
            raise RuntimeError("Launcher exited unsuccessfully")
        if heartbeat.exists():
            raise RuntimeError("Fallback heartbeat survived process exit")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        process.stdout.close()
        process.stderr.close()
        if not process.stdin.closed:
            process.stdin.close()


def main():
    if BACKUP.exists():
        raise RuntimeError("Existing smoke-test backup requires manual inspection")
    if not CONFIG.is_file() or not LAUNCHER.is_file() or not RELAY.is_file():
        raise RuntimeError("Claude Desktop config, launcher, or relay missing")
    original = CONFIG.read_bytes()
    config = json.loads(original)
    node = config.get("mcpServers", {}).get(NAME)
    if not isinstance(node, dict) or not isinstance(node.get("command"), str):
        raise RuntimeError("Claude Desktop web-search-prime node missing or invalid")
    if set(node) - {"command", "args", "env"}:
        raise RuntimeError("Unexpected web-search-prime node shape")
    if not isinstance(node.get("args", []), list) or not isinstance(node.get("env", {}), dict):
        raise RuntimeError("Invalid web-search-prime arguments or environment")
    entry = {"name": NAME, "description": "", "transport": "stdio", "command": node["command"],
             "args": node.get("args", []), "env": node.get("env", {}), "url": "", "headers": {},
             "assigned_agents": ["claude_desktop"], "created_at": "", "updated_at": ""}
    canonical = {"command": entry["command"], "args": entry["args"]}
    if entry["env"]:
        canonical["env"] = entry["env"]
    if node != canonical:
        raise RuntimeError("Claude Desktop entry has an unsupported shape")
    with catalog_connection() as database:
        original_row = database.execute(
            "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (SETTING,),
        ).fetchone()
        if original_row is None:
            raise RuntimeError("Central MCP catalog missing")
        migration = database.execute(
            "SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'",
        ).fetchone()
        selected = database.execute(
            "SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'",
        ).fetchone()
        if migration is None or json.loads(migration[0]).get("completed") is not True:
            raise RuntimeError("Central MCP catalog migration incomplete")
        if selected is None or Path(json.loads(selected[0])).resolve() != CONFIG.resolve():
            raise RuntimeError("Claude Desktop path selection changed")
        if any(item.get("name") == NAME for item in read_catalog(database)):
            raise RuntimeError("web-search-prime now exists in the catalog; inspect before retrying")
    shutil.copy2(CONFIG, BACKUP)
    if BACKUP.read_bytes() != original or CONFIG.read_bytes() != original:
        raise RuntimeError("Claude Desktop config changed during backup; backup retained")
    expect_fallback = "--fallback" in sys.argv[1:]
    if set(sys.argv[1:]) - {"--fallback"}:
        raise RuntimeError("Supported option: --fallback")
    assignment = str(uuid.uuid4())
    proxy = {"command": str(LAUNCHER), "args": ["--mcp-relay", assignment]}
    updated = json.loads(original)
    updated["mcpServers"][NAME] = proxy
    installed_bytes = json.dumps(updated, ensure_ascii=False, indent=2).encode("utf-8")
    manifest_path = OBSERVATIONS / f"{assignment}.json"
    entry_digest = hashlib.sha256(json_bytes(entry)).hexdigest()
    catalog_added = False
    installed = False
    expected_catalog_row = None
    try:
        with catalog_connection() as database:
            database.execute("BEGIN IMMEDIATE")
            row = database.execute(
                "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (SETTING,),
            ).fetchone()
            if row != original_row:
                raise RuntimeError("Catalog changed during test setup")
            entries = read_catalog(database)
            if any(item.get("name") == NAME for item in entries):
                raise RuntimeError("Catalog changed during test setup")
            entries.append(entry)
            update_catalog(database, entries)
            expected_catalog_row = database.execute(
                "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (SETTING,),
            ).fetchone()
        catalog_added = True
        if CONFIG.read_bytes() != original:
            raise RuntimeError("Claude Desktop config changed during test setup")
        atomic_bytes(CONFIG, installed_bytes)
        installed = True
        OBSERVATIONS.mkdir(parents=True, exist_ok=True)
        manifest = {"id": assignment, "name": NAME, "agent": "claude_desktop", "config_path": str(CONFIG),
                    "fingerprint": hashlib.sha256(json_bytes(canonical)).hexdigest(),
                    "proxy_command": str(LAUNCHER),
                    "relay_command": str(RELAY.with_name("missing-relay.exe") if expect_fallback else RELAY),
                    "phase": "active"}
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8", newline="\n")
        run_call(assignment, expect_fallback)
    finally:
        manifest_path.unlink(missing_ok=True)
        config_restored = False
        if installed and CONFIG.read_bytes() == installed_bytes:
            os.replace(BACKUP, CONFIG)
            config_restored = CONFIG.read_bytes() == original
        elif not installed and CONFIG.read_bytes() == original:
            BACKUP.unlink()
            config_restored = True
        if catalog_added:
            with catalog_connection() as database:
                database.execute("BEGIN IMMEDIATE")
                entries = read_catalog(database)
                matches = [item for item in entries if item.get("name") == NAME]
                if len(matches) != 1 or hashlib.sha256(json_bytes(matches[0])).hexdigest() != entry_digest:
                    raise RuntimeError("Catalog changed concurrently; temporary entry retained for inspection")
                row = database.execute(
                    "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (SETTING,),
                ).fetchone()
                if row != expected_catalog_row:
                    raise RuntimeError("Catalog changed concurrently; temporary entry retained for inspection")
                database.execute(
                    "UPDATE app_settings SET value_json = ?, updated_at = ? WHERE setting_key = ?",
                    (*original_row, SETTING),
                )
        if not config_restored:
            raise RuntimeError("Claude Desktop config changed concurrently; original backup retained")
    print("claude_desktop_configuration_restored", CONFIG.read_bytes() == original)
    with catalog_connection() as database:
        print("temporary_catalog_entry_removed", not any(item.get("name") == NAME for item in read_catalog(database)))


if __name__ == "__main__":
    main()
