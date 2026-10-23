"""Run the ignored Rust observation toggle test against Claude Desktop.

Registers web-search-prime temporarily and restores config bytes and catalog
only when their test versions are still present. Never prints credentials.
"""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess


SCRIPT = Path(__file__).resolve().with_name("smoke-web-search-fallback.py")
spec = importlib.util.spec_from_file_location("web_search_smoke", SCRIPT)
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
BACKUP = smoke.CONFIG.with_name(smoke.CONFIG.name + ".vibe-toggle-smoke-backup")


def run():
    if BACKUP.exists() or smoke.BACKUP.exists():
        raise RuntimeError("Smoke test backup exists; inspect before retrying")
    original = smoke.CONFIG.read_bytes()
    source = json.loads(original)
    node = source.get("mcpServers", {}).get(smoke.NAME)
    if not isinstance(node, dict) or set(node) - {"command", "args", "env"}:
        raise RuntimeError("Original web-search-prime node has unexpected shape")
    if not isinstance(node.get("command"), str) or not isinstance(node.get("args", []), list) or not isinstance(node.get("env", {}), dict):
        raise RuntimeError("Original web-search-prime node is invalid")
    with smoke.catalog_connection() as db:
        original_row = db.execute(
            "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,),
        ).fetchone()
        selected = db.execute(
            "SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'",
        ).fetchone()
        migrated = db.execute(
            "SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'",
        ).fetchone()
        if not original_row or not selected or not migrated or json.loads(migrated[0]).get("completed") is not True:
            raise RuntimeError("Catalog or path selection unavailable")
        if Path(json.loads(selected[0])).resolve() != smoke.CONFIG.resolve():
            raise RuntimeError("Selected Claude Desktop path differs from test target")
        if any(entry.get("name") == smoke.NAME for entry in smoke.read_catalog(db)):
            raise RuntimeError("web-search-prime already exists in central catalog")
    if any(json.loads(p.read_text(encoding="utf-8")).get("name") == smoke.NAME
           and json.loads(p.read_text(encoding="utf-8")).get("agent") == "claude_desktop"
           for p in smoke.OBSERVATIONS.glob("*.json")):
        raise RuntimeError("An existing observation must be resolved before this test")
    shutil.copy2(smoke.CONFIG, BACKUP)
    if BACKUP.read_bytes() != original or smoke.CONFIG.read_bytes() != original:
        raise RuntimeError("Config changed during backup; backup retained")

    entry = {"name": smoke.NAME, "description": "", "transport": "stdio", "command": node["command"],
             "args": node.get("args", []), "env": node.get("env", {}), "url": "", "headers": {},
             "assigned_agents": ["claude_desktop"], "created_at": "", "updated_at": ""}
    expected_row = None
    registered = False
    test_result = None
    restored = False
    try:
        with smoke.catalog_connection() as db:
            db.execute("BEGIN IMMEDIATE")
            row = db.execute(
                "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,),
            ).fetchone()
            if row != original_row or smoke.CONFIG.read_bytes() != original:
                raise RuntimeError("Catalog or Claude Desktop config changed during setup")
            entries = smoke.read_catalog(db)
            entries.append(entry)
            smoke.update_catalog(db, entries)
            expected_row = db.execute(
                "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,),
            ).fetchone()
        registered = True
        env = os.environ.copy()
        env["VIBE_OBSERVATION_TOGGLE_SMOKE"] = "1"
        command = ["cargo", "test", "--lib", "smoke_claude_desktop_observation_toggle",
                   "--", "--ignored", "--nocapture"]
        test_result = subprocess.run(command, cwd=smoke.ROOT / "src-tauri", env=env, timeout=540, check=False)
    finally:
        current = smoke.CONFIG.read_bytes()
        current_data = json.loads(current)
        manifest_files = [p for p in smoke.OBSERVATIONS.glob("*.json")
                          if json.loads(p.read_text(encoding="utf-8")).get("name") == smoke.NAME
                          and json.loads(p.read_text(encoding="utf-8")).get("agent") == "claude_desktop"]
        if current_data == source and not manifest_files:
            os.replace(BACKUP, smoke.CONFIG)
            restored = smoke.CONFIG.read_bytes() == original
        elif (current_data.get("mcpServers") == source.get("mcpServers")
              and not manifest_files):
            # An Agent may update unrelated settings while the test runs.
            # Preserve those changes and remove only our catalog entry below.
            restored = True
        elif len(manifest_files) == 1:
            manifest = json.loads(manifest_files[0].read_text(encoding="utf-8"))
            expected = json.loads(original)
            expected["mcpServers"][smoke.NAME] = {
                "command": manifest.get("proxy_command"), "args": ["--mcp-relay", manifest.get("id")],
            }
            if current_data == expected and manifest.get("config_path") == str(smoke.CONFIG):
                os.replace(BACKUP, smoke.CONFIG)
                manifest_files[0].unlink()
                restored = smoke.CONFIG.read_bytes() == original
        if registered and restored:
            with smoke.catalog_connection() as db:
                db.execute("BEGIN IMMEDIATE")
                row = db.execute(
                    "SELECT value_json, updated_at FROM app_settings WHERE setting_key = ?", (smoke.SETTING,),
                ).fetchone()
                if row != expected_row:
                    raise RuntimeError("Catalog changed concurrently; temporary entry retained")
                db.execute(
                    "UPDATE app_settings SET value_json = ?, updated_at = ? WHERE setting_key = ?",
                    (*original_row, smoke.SETTING),
                )
        if not restored:
            raise RuntimeError("Config changed during test; backup retained for inspection")
        if BACKUP.exists() and smoke.CONFIG.read_bytes() == current:
            BACKUP.unlink()
    print("original_mcp_servers_restored", restored, flush=True)
    if test_result is None or test_result.returncode != 0:
        raise RuntimeError("Rust observation toggle test did not pass")


if __name__ == "__main__":
    run()
