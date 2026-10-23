"""Opt-in local smoke test of the zai-mcp-server fallback path.

Temporarily switches only Claude Desktop's zai-mcp-server node and restores its
original configuration on exit. Never prints catalog commands or credentials.
"""

import hashlib
import json
import os
import pathlib
import queue
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid


ROOT = pathlib.Path(__file__).resolve().parent.parent
CONFIG_ROOT = pathlib.Path(os.environ["APPDATA"]) / "agent-manager"
OBSERVATIONS = CONFIG_ROOT / "mcp-observation"
AGENT = "claude_desktop"
CONFIG = pathlib.Path(os.environ["LOCALAPPDATA"]) / "Claude-3p" / "claude_desktop_config.json"
BACKUP = CONFIG.with_name(CONFIG.name + ".vibe-fallback-smoke-backup")
LAUNCHER = (ROOT / "src-tauri" / "mcp-launcher" / "target" / "debug" / "mcp-launcher.exe").resolve()
RELAY = (ROOT / "src-tauri" / "target" / "debug" / "agent-manager.exe").resolve()
NAME = "zai-mcp-server"
TEST_IMAGE = ROOT / "src-tauri" / "icons" / "32x32.png"


def atomic_bytes(path, content):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as file:
        temporary = pathlib.Path(file.name)
        file.write(content)
        file.flush()
        os.fsync(file.fileno())
    try:
        shutil.copymode(path, temporary)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def responses_for(command, assignment, expect_fallback):
    messages = queue.Queue()
    process = subprocess.Popen(
        [str(command), "--mcp-relay", assignment],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env={**os.environ, "npm_config_cache": os.environ.get("VIBE_MCP_TEST_NPM_CACHE", str(ROOT / ".cache" / "npm"))},
    )

    def read_stdout():
        for line in process.stdout:
            try:
                messages.put(json.loads(line))
            except (UnicodeError, ValueError):
                pass
        messages.put(None)

    threading.Thread(target=read_stdout, daemon=True).start()
    try:
        for request in [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-03-26", "capabilities": {},
                "clientInfo": {"name": "fallback-smoke", "version": "1.0"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        ]:
            process.stdin.write((json.dumps(request, separators=(",", ":")) + "\n").encode())
            process.stdin.flush()
        found = {}
        deadline = time.monotonic() + 60
        while len(found) < 2 and time.monotonic() < deadline:
            try:
                response = messages.get(timeout=min(2, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                if process.poll() is not None:
                    break
                continue
            if response is None:
                break
            if isinstance(response, dict) and response.get("id") in (1, 2):
                found[response["id"]] = response
        if len(found) != 2 or any("error" in response for response in found.values()):
            raise RuntimeError("MCP initialization or tools/list did not succeed")
        tools = found[2].get("result", {}).get("tools", [])
        if len(tools) != 8:
            raise RuntimeError("Unexpected MCP tool count")
        heartbeat = OBSERVATIONS / "fallback" / assignment / f"{process.pid}.json"
        if expect_fallback:
            deadline = time.monotonic() + 5
            while not heartbeat.is_file() and time.monotonic() < deadline:
                time.sleep(0.05)
            if not heartbeat.is_file():
                raise RuntimeError("Fallback heartbeat was not written")
            state = json.loads(heartbeat.read_text(encoding="utf-8"))
            if state.get("assignment_id") != assignment or state.get("pid") != process.pid:
                raise RuntimeError("Fallback heartbeat does not match this process")
        elif heartbeat.is_file():
            raise RuntimeError("Healthy relay unexpectedly wrote a fallback heartbeat")
        database = sqlite3.connect(f"file:{(CONFIG_ROOT / 'telemetry.sqlite3').as_posix()}?mode=ro", uri=True)
        try:
            active = database.execute(
                "SELECT COUNT(*) FROM mcp_proxy_connections WHERE assignment_id = ?",
                (hashlib.sha256(assignment.encode()).hexdigest(),)
            ).fetchone()[0]
        finally:
            database.close()
        if (active > 0) == expect_fallback:
            raise RuntimeError("Relay connection state disagrees with fallback state")
        if not TEST_IMAGE.is_file() or not any(tool.get("name") == "analyze_image" for tool in tools):
            raise RuntimeError("Local test image or ZAI analyze_image tool missing")
        call = {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "analyze_image", "arguments": {
                "image_source": str(TEST_IMAGE),
                "prompt": "Describe the visible shapes and colors in this application icon briefly.",
            }}}
        process.stdin.write((json.dumps(call, separators=(",", ":")) + "\n").encode())
        process.stdin.flush()
        response = None
        deadline = time.monotonic() + 90
        while response is None and time.monotonic() < deadline:
            try:
                message = messages.get(timeout=min(2, max(0.01, deadline - time.monotonic())))
            except queue.Empty:
                if process.poll() is not None:
                    break
                continue
            if message is None:
                break
            if isinstance(message, dict) and message.get("id") == 3:
                response = message
        if response is None:
            raise RuntimeError("ZAI tools/call produced no response")
        result = response.get("result")
        content = result.get("content") if isinstance(result, dict) else None
        text = " ".join(item.get("text", "") for item in content if item.get("type") == "text") if isinstance(content, list) else ""
        tool_error = "error" in response or not isinstance(result, dict) or result.get("isError") is True or not text.strip()
        quota_exhausted = tool_error and "http 429" in text.lower() and '"code":"1310"' in text.replace(" ", "")
        print(json.dumps({"scenario": "fallback" if expect_fallback else "relay", "initialized": True,
                          "tools": len(tools), "relay_connections": active, "fallback_heartbeat": heartbeat.is_file(),
                          "tool_call": "analyze_image", "tool_call_success": not tool_error,
                          "upstream_quota_exhausted": quota_exhausted,
                          "result_chars": len(text) if not tool_error else 0}), flush=True)
        if tool_error and not quota_exhausted:
            raise RuntimeError("ZAI tools/call failed for a reason other than the known upstream quota")
        process.stdin.close()
        process.wait(timeout=15)
        diagnostic = process.stderr.read().decode(errors="replace")
        if process.returncode != 0:
            raise RuntimeError("Launcher exited unsuccessfully" +
                               (" during forwarding" if "connection ended unexpectedly" in diagnostic else ""))
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
    if not LAUNCHER.is_file() or not RELAY.is_file():
        raise RuntimeError("Build the launcher and relay before running this test")
    if BACKUP.exists():
        raise RuntimeError("Existing smoke-test backup requires manual inspection")
    database = sqlite3.connect(f"file:{(CONFIG_ROOT / 'telemetry.sqlite3').as_posix()}?mode=ro", uri=True)
    try:
        row = database.execute("SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog'").fetchone()
        entry = next(item for item in json.loads(row[0]) if item.get("name") == NAME)
        migration = database.execute("SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'").fetchone()
        if json.loads(migration[0]).get("completed") is not True:
            raise RuntimeError("Central MCP catalog has not been migrated")
        selected = database.execute("SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'").fetchone()
        if selected is None or pathlib.Path(json.loads(selected[0])).resolve() != CONFIG.resolve():
            raise RuntimeError("Selected Claude Desktop MCP configuration differs from the test target")
    finally:
        database.close()
    if entry.get("transport") not in (None, "", "stdio") or AGENT not in entry.get("assigned_agents", []):
        raise RuntimeError("ZAI MCP is not assigned to Claude Desktop as stdio")
    original = CONFIG.read_bytes()
    config = json.loads(original)
    canonical = {"command": entry["command"], "args": entry.get("args", [])}
    if entry.get("env"):
        canonical["env"] = entry["env"]
    if entry.get("description"):
        canonical["description"] = entry["description"]
    if config.get("mcpServers", {}).get(NAME) != canonical:
        raise RuntimeError("Claude Desktop MCP entry differs from the catalog")
    fingerprint = hashlib.sha256(json.dumps(canonical, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()).hexdigest()
    if not OBSERVATIONS.is_dir():
        OBSERVATIONS.mkdir(parents=True)
    shutil.copy2(CONFIG, BACKUP)
    if BACKUP.read_bytes() != original or CONFIG.read_bytes() != original:
        raise RuntimeError("Claude Desktop config changed during backup; inspect backup before retrying")
    manifest_paths = []
    installed_bytes = None
    try:
        # Exercise the missing-relay path; upstream usage may be limited.
        for expect_fallback in (True,):
            assignment = str(uuid.uuid4())
            proxy = {"command": str(LAUNCHER), "args": ["--mcp-relay", assignment]}
            updated = json.loads(original)
            updated["mcpServers"][NAME] = proxy
            new_bytes = json.dumps(updated, ensure_ascii=False, indent=2).encode()
            atomic_bytes(CONFIG, new_bytes)
            installed_bytes = new_bytes
            manifest = {
                "id": assignment, "name": NAME, "agent": AGENT, "config_path": str(CONFIG),
                "fingerprint": fingerprint, "proxy_command": str(LAUNCHER),
                "relay_command": str(RELAY if not expect_fallback else RELAY.with_name("missing-relay.exe")),
                "phase": "active",
            }
            manifest_path = OBSERVATIONS / f"{assignment}.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8", newline="\n")
            manifest_paths.append(manifest_path)
            responses_for(LAUNCHER, assignment, expect_fallback)
            manifest_path.unlink()
            manifest_paths.remove(manifest_path)
    finally:
        for manifest_path in manifest_paths:
            manifest_path.unlink(missing_ok=True)
        if CONFIG.read_bytes() == installed_bytes:
            os.replace(BACKUP, CONFIG)
        else:
            print("Claude Desktop config changed concurrently; original backup retained", file=sys.stderr)
            raise RuntimeError("Cannot safely restore Claude Desktop config automatically")
    print("claude_desktop_configuration_restored", CONFIG.read_bytes() == original)


if __name__ == "__main__":
    main()
