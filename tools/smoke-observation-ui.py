"""Prepare, drift, and clean up a temporary UI observation test entry.

Reads the existing Claude Desktop MCP node. Never prints command, args or env.
"""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import uuid


SCRIPT = Path(__file__).resolve().with_name("smoke-web-search-fallback.py")
spec = importlib.util.spec_from_file_location("web_search_smoke", SCRIPT)
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
BACKUP = smoke.CONFIG.with_name(smoke.CONFIG.name + ".vibe-observation-ui-backup")


def catalog_entry(node):
    return {"name": smoke.NAME, "description": "", "transport": "stdio",
            "command": node["command"], "args": node.get("args", []),
            "env": node.get("env", {}), "url": "", "headers": {},
            "assigned_agents": ["claude_desktop"], "created_at": "", "updated_at": ""}


def manifests():
    return [(path, data) for path in smoke.OBSERVATIONS.glob("*.json")
            if (data := json.loads(path.read_text(encoding="utf-8"))).get("name") == smoke.NAME
            and data.get("agent") == "claude_desktop"]


def validate_node(node):
    if (not isinstance(node, dict) or set(node) - {"command", "args", "env"}
            or not isinstance(node.get("command"), str)
            or not isinstance(node.get("args", []), list)
            or not isinstance(node.get("env", {}), dict)):
        raise RuntimeError("Test server node is missing or unsupported")


def original():
    if not BACKUP.is_file():
        raise RuntimeError("UI test backup is missing")
    data = json.loads(BACKUP.read_bytes())
    node = data.get("mcpServers", {}).get(smoke.NAME)
    validate_node(node)
    return node


def check_selected(db):
    selected = db.execute(
        "SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'"
    ).fetchone()
    migration = db.execute(
        "SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'"
    ).fetchone()
    if (not selected or Path(json.loads(selected[0])).resolve() != smoke.CONFIG.resolve()
            or not migration or json.loads(migration[0]).get("completed") is not True):
        raise RuntimeError("Claude Desktop selection or central migration changed")


def setup():
    if BACKUP.exists() or manifests():
        raise RuntimeError("An existing backup or observation must be inspected")
    initial = smoke.CONFIG.read_bytes()
    data = json.loads(initial)
    node = data.get("mcpServers", {}).get(smoke.NAME)
    validate_node(node)
    entry = catalog_entry(node)
    with smoke.catalog_connection() as db:
        check_selected(db)
        if any(item.get("name") == smoke.NAME for item in smoke.read_catalog(db)):
            raise RuntimeError("Test server already exists in central catalog")
    shutil.copy2(smoke.CONFIG, BACKUP)
    if BACKUP.read_bytes() != initial or smoke.CONFIG.read_bytes() != initial:
        raise RuntimeError("Config changed during backup; backup retained")
    with smoke.catalog_connection() as db:
        db.execute("BEGIN IMMEDIATE")
        check_selected(db)
        if smoke.CONFIG.read_bytes() != initial:
            raise RuntimeError("Config changed during setup; backup retained")
        entries = smoke.read_catalog(db)
        if any(item.get("name") == smoke.NAME for item in entries):
            raise RuntimeError("Catalog changed during setup; backup retained")
        entries.append(entry)
        smoke.update_catalog(db, entries)
    print("temporary_ui_entry_registered True", flush=True)


def drift():
    node = original()
    records = manifests()
    if len(records) != 1:
        raise RuntimeError("Expected one active observation manifest")
    _, manifest = records[0]
    if manifest.get("config_path") != str(smoke.CONFIG) or manifest.get("phase") != "active":
        raise RuntimeError("Observation manifest does not match test config")
    with smoke.catalog_connection() as db:
        check_selected(db)
        if catalog_entry(node) not in smoke.read_catalog(db):
            raise RuntimeError("Temporary catalog entry changed")
    before = smoke.CONFIG.read_bytes()
    data = json.loads(before)
    proxy = {"command": manifest.get("proxy_command"),
             "args": ["--mcp-relay", manifest.get("id")]}
    if data.get("mcpServers", {}).get(smoke.NAME) != proxy:
        raise RuntimeError("Expected the active relay node before drift")
    data["mcpServers"][smoke.NAME] = node
    if smoke.CONFIG.read_bytes() != before:
        raise RuntimeError("Config changed during drift setup")
    smoke.atomic_bytes(smoke.CONFIG, json.dumps(data, ensure_ascii=False, indent=2).encode("utf-8"))
    if json.loads(smoke.CONFIG.read_bytes()).get("mcpServers", {}).get(smoke.NAME) != node:
        raise RuntimeError("Drift write could not be verified")
    print("original_node_reinstalled_for_retry True", flush=True)


def cleanup():
    node = original()
    records = manifests()
    if records:
        raise RuntimeError("Observation remains active; disable it in the app before cleanup")
    with smoke.catalog_connection() as db:
        check_selected(db)
        entries = smoke.read_catalog(db)
        if sum(item.get("name") == smoke.NAME for item in entries) != 1 or catalog_entry(node) not in entries:
            raise RuntimeError("Temporary catalog entry changed; backup retained")
        before = smoke.CONFIG.read_bytes()
        data = json.loads(before)
        if data.get("mcpServers", {}).get(smoke.NAME) != node:
            raise RuntimeError("Target MCP node changed; backup retained")
        db.execute("BEGIN IMMEDIATE")
        current = smoke.read_catalog(db)
        if current != entries or smoke.CONFIG.read_bytes() != before:
            raise RuntimeError("Catalog or config changed during cleanup; backup retained")
        current.remove(catalog_entry(node))
        smoke.update_catalog(db, current)
    if smoke.CONFIG.read_bytes() == BACKUP.read_bytes():
        BACKUP.unlink()
    else:
        # Keep unrelated Agent edits; the original MCP node has been checked above.
        BACKUP.unlink()
    print("temporary_ui_entry_removed True", flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in {"setup", "drift", "cleanup"}:
        raise SystemExit("Usage: smoke-observation-ui.py setup|drift|cleanup")
    {"setup": setup, "drift": drift, "cleanup": cleanup}[sys.argv[1]]()
