"""Test MCP pre-GO direct fallback with a temporary, uniquely named assignment.

Pass --real-web-search to call the configured web-search-prime server instead of
an in-process stub. Existing assignments remain intact. Output omits configured
commands, credentials, and tool result text.
"""

import hashlib
import importlib.util
import json
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid


SPEC = importlib.util.spec_from_file_location(
    "mcp_smoke_helpers", Path(__file__).with_name("smoke-web-search-fallback.py")
)
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)
NAME = "vibe-fallback-smoke-" + uuid.uuid4().hex[:12]
STUB = '''import json,sys
for line in sys.stdin:
    req=json.loads(line)
    if "id" not in req: continue
    method=req.get("method")
    if method=="initialize": result={"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fallback-smoke","version":"1"}}
    elif method=="tools/list": result={"tools":[{"name":"ping","description":"Test response","inputSchema":{"type":"object","properties":{}}}]}
    elif method=="tools/call": result={"content":[{"type":"text","text":"pong"}]}
    else: result={}
    print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":result}),flush=True)
'''


def matching_node(config, expected):
    return config.get("mcpServers", {}).get(NAME) == expected


def main():
    real_service = sys.argv[1:] == ["--real-web-search"]
    if sys.argv[1:] not in ([], ["--real-web-search"]):
        raise RuntimeError("Supported option: --real-web-search")
    if not smoke.LAUNCHER.is_file() or not smoke.CONFIG.is_file():
        raise RuntimeError("Launcher or selected Agent config unavailable")
    assignment = str(uuid.uuid4())
    original = {"command": sys.executable, "args": ["-u", "-c", STUB]}
    if real_service:
        with smoke.catalog_connection() as db:
            sources = [item for item in smoke.read_catalog(db) if item.get("name") == "web-search-prime"]
        if len(sources) != 1 or sources[0].get("transport") not in ("", "stdio"):
            raise RuntimeError("Real MCP service unavailable or ambiguous")
        source = sources[0]
        if not isinstance(source.get("command"), str) or not source["command"]:
            raise RuntimeError("Real MCP service has no command")
        if not isinstance(source.get("args"), list) or not isinstance(source.get("env"), dict):
            raise RuntimeError("Real MCP service has invalid arguments or environment")
        original = {"command": source["command"], "args": source["args"]}
        if source["env"]:
            original["env"] = source["env"]
    entry = {"name": NAME, "description": "", "transport": "stdio", "command": original["command"],
             "args": original["args"], "env": original.get("env", {}), "url": "", "headers": {},
             "assigned_agents": ["claude_desktop"], "created_at": "", "updated_at": ""}
    proxy = {"command": str(smoke.LAUNCHER), "args": ["--mcp-relay", assignment]}
    manifest_path = smoke.OBSERVATIONS / f"{assignment}.json"
    heartbeat = smoke.OBSERVATIONS / "fallback" / assignment
    catalog_registered = False
    node_registered = False
    installed_bytes = None
    original_catalog_row = None
    expected_catalog_row = None
    process = None
    with tempfile.TemporaryDirectory(prefix="mcp-fallback-test-", dir=smoke.ROOT / "tools") as temp:
        missing_relay = str(Path(temp) / "missing-relay.exe")
        try:
            with smoke.catalog_connection() as db:
                selected = db.execute("SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'").fetchone()
                migration = db.execute("SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'").fetchone()
                if (not selected or Path(json.loads(selected[0])).resolve() != smoke.CONFIG.resolve()
                        or not migration or json.loads(migration[0]).get("completed") is not True):
                    raise RuntimeError("Selected Claude Desktop config or migration changed")
                if any(item.get("name") == NAME for item in smoke.read_catalog(db)):
                    raise RuntimeError("Test name already in catalog")
                before = smoke.CONFIG.read_bytes()
                config = json.loads(before)
                if not isinstance(config.get("mcpServers"), dict) or NAME in config["mcpServers"]:
                    raise RuntimeError("Test name already in Agent config")
                db.execute("BEGIN IMMEDIATE")
                if smoke.CONFIG.read_bytes() != before:
                    raise RuntimeError("Agent config changed before test registration")
                original_catalog_row = db.execute(
                    "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,)
                ).fetchone()
                entries = smoke.read_catalog(db)
                entries.append(entry)
                smoke.update_catalog(db, entries)
                expected_catalog_row = db.execute(
                    "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,)
                ).fetchone()
            catalog_registered = True
            config["mcpServers"][NAME] = proxy
            installed_bytes = json.dumps(config, ensure_ascii=False, indent=2).encode("utf-8")
            if smoke.CONFIG.read_bytes() != before:
                raise RuntimeError("Agent config changed before test installation")
            smoke.atomic_bytes(smoke.CONFIG, installed_bytes)
            node_registered = True
            fingerprint = hashlib.sha256(smoke.json_bytes(original)).hexdigest()
            manifest = {"id": assignment, "name": NAME, "agent": "claude_desktop",
                        "config_path": str(smoke.CONFIG), "fingerprint": fingerprint,
                        "proxy_command": str(smoke.LAUNCHER), "relay_command": missing_relay,
                        "phase": "active"}
            smoke.OBSERVATIONS.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8", newline="\n")
            process = subprocess.Popen([str(smoke.LAUNCHER), "--mcp-relay", assignment],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL)
            messages = queue.Queue()

            def read_output():
                for line in process.stdout:
                    try:
                        messages.put(json.loads(line))
                    except (ValueError, UnicodeError):
                        pass
                messages.put(None)

            threading.Thread(target=read_output, daemon=True).start()

            def send(payload):
                process.stdin.write((json.dumps(payload) + "\n").encode("utf-8"))
                process.stdin.flush()

            def request(number, method, params):
                send({"jsonrpc": "2.0", "id": number, "method": method, "params": params})
                deadline = time.monotonic() + (90 if real_service else 15)
                while time.monotonic() < deadline:
                    try:
                        reply = messages.get(timeout=min(1, max(0.01, deadline - time.monotonic())))
                    except queue.Empty:
                        continue
                    if reply is None:
                        break
                    if reply.get("id") == number:
                        return reply
                raise RuntimeError("MCP test request did not return")

            initialized = request(1, "initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                                     "clientInfo": {"name": "fallback-test", "version": "1"}})
            send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            listed = request(2, "tools/list", {})
            deadline = time.monotonic() + 5
            heartbeat_path = heartbeat / f"{process.pid}.json"
            while not heartbeat_path.is_file() and time.monotonic() < deadline:
                time.sleep(0.05)
            fallback_active = heartbeat_path.is_file()
            heartbeat_valid = False
            if fallback_active:
                state = json.loads(heartbeat_path.read_text(encoding="utf-8"))
                heartbeat_valid = state.get("assignment_id") == assignment and state.get("pid") == process.pid
            tool = "web_search_prime" if real_service else "ping"
            arguments = ({"search_query": "Model Context Protocol official documentation", "location": "us",
                          "content_size": "medium"} if real_service else {})
            called = request(3, "tools/call", {"name": tool, "arguments": arguments})
            with sqlite3.connect(f"file:{smoke.DATABASE.as_posix()}?mode=ro", uri=True) as db:
                observed = db.execute("SELECT COUNT(*) FROM mcp_call_observations WHERE source = 'stdio_proxy' AND server_name = ?", (NAME,)).fetchone()[0]
                connections = db.execute("SELECT COUNT(*) FROM mcp_proxy_connections WHERE server_name = ?", (NAME,)).fetchone()[0]
            server_info = initialized.get("result", {}).get("serverInfo", {})
            tools = listed.get("result", {}).get("tools", [])
            content = called.get("result", {}).get("content", [])
            response_ok = ("error" not in called and called.get("result", {}).get("isError") is not True
                           and isinstance(content, list) and any(isinstance(part, dict)
                           and isinstance(part.get("text"), str) and part["text"].strip() for part in content))
            success = ("error" not in initialized and isinstance(server_info.get("name"), str)
                       and (real_service or server_info["name"] == "fallback-smoke")
                       and isinstance(tools, list) and any(isinstance(item, dict) and item.get("name") == tool for item in tools)
                       and (response_ok if real_service else content == [{"type": "text", "text": "pong"}])
                       and heartbeat_valid and observed == 0 and connections == 0)
            process.stdin.close()
            process.wait(timeout=15)
            print("real_service", real_service, "fallback_active", heartbeat_valid, "tool_call_succeeded", success,
                  "observation_paused", observed == 0 and connections == 0,
                  "heartbeat_removed", not heartbeat_path.exists(),
                  "launcher_exit", process.returncode, flush=True)
            if not success or process.returncode != 0 or (heartbeat / f"{process.pid}.json").exists():
                raise RuntimeError("Direct fallback failed")
        finally:
            if process is not None:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                process.stdout.close()
                if not process.stdin.closed:
                    process.stdin.close()
            if node_registered:
                before_cleanup = smoke.CONFIG.read_bytes()
                config = json.loads(before_cleanup)
                if not matching_node(config, proxy):
                    raise RuntimeError("Test node changed; assignment retained for inspection")
                if before_cleanup == installed_bytes:
                    smoke.atomic_bytes(smoke.CONFIG, before)
                else:
                    del config["mcpServers"][NAME]
                    if smoke.CONFIG.read_bytes() != before_cleanup:
                        raise RuntimeError("Agent config changed during cleanup; assignment retained")
                    smoke.atomic_bytes(smoke.CONFIG, json.dumps(config, ensure_ascii=False, indent=2).encode("utf-8"))
            if catalog_registered:
                with smoke.catalog_connection() as db:
                    db.execute("BEGIN IMMEDIATE")
                    entries = smoke.read_catalog(db)
                    matches = [item for item in entries if item.get("name") == NAME]
                    if matches != [entry]:
                        raise RuntimeError("Test catalog entry changed; retained for inspection")
                    row = db.execute(
                        "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,)
                    ).fetchone()
                    if row == expected_catalog_row:
                        db.execute(
                            "UPDATE app_settings SET value_json = ?, updated_at = ? WHERE setting_key = ?",
                            (*original_catalog_row, smoke.SETTING),
                        )
                    else:
                        entries.remove(entry)
                        smoke.update_catalog(db, entries)
            manifest_path.unlink(missing_ok=True)
            print("test_node_removed", NAME not in json.loads(smoke.CONFIG.read_bytes()).get("mcpServers", {}),
                  "test_manifest_removed", not manifest_path.exists(), flush=True)


if __name__ == "__main__":
    main()
