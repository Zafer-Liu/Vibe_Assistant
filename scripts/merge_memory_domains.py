"""Merge one approved same-name label pair, with a metadata backup and no model calls.

Default is read-only. --apply requires --backup-dir. Does not upload to cloud,
change memory content, or advance user_version under an older running binary.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import uuid


def content_fingerprint(db):
    digest = hashlib.sha256()
    count = 0
    for row in db.execute("SELECT * FROM local_memory_items ORDER BY id"):
        digest.update(json.dumps(tuple(row), ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
        digest.update(b"\n")
        count += 1
    return count, digest.hexdigest()


def records(db, query, args=()):
    return [dict(row) for row in db.execute(query, args)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path)
    parser.add_argument("--source-id", required=True)
    parser.add_argument("--target-id", required=True)
    parser.add_argument("--backup-dir", type=Path)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if args.source_id == args.target_id:
        parser.error("source and target must differ")
    if args.apply and args.backup_dir is None:
        parser.error("--apply requires --backup-dir")
    database = args.database
    if database is None:
        database = Path(os.environ["APPDATA"]) / "agent-manager" / "telemetry.sqlite3"
    mode = "rw" if args.apply else "ro"
    with sqlite3.connect(database.resolve().as_uri() + f"?mode={mode}", uri=True, timeout=8) as db:
        db.row_factory = sqlite3.Row
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("PRAGMA busy_timeout=8000")
        if not args.apply:
            db.execute("PRAGMA query_only=ON")
        db.execute("BEGIN IMMEDIATE" if args.apply else "BEGIN")
        try:
            source = db.execute("SELECT * FROM memory_domains WHERE id=?", (args.source_id,)).fetchone()
            target = db.execute("SELECT * FROM memory_domains WHERE id=?", (args.target_id,)).fetchone()
            if source is None and target is not None:
                has_merges = db.execute("SELECT 1 FROM sqlite_master WHERE name='memory_domain_merges'").fetchone()
                redirect = db.execute("SELECT * FROM memory_domain_merges WHERE source_id=?", (args.source_id,)).fetchone() if has_merges else None
                retired = db.execute("SELECT 1 FROM memory_domain_tombstones WHERE domain_id=?", (args.source_id,)).fetchone()
                if redirect and redirect["target_id"] == args.target_id and retired:
                    remaining = db.execute("SELECT count(*) FROM local_memory_domains WHERE domain_id=?", (args.source_id,)).fetchone()[0]
                    if remaining:
                        raise RuntimeError("Retired label still has memberships; inspect before repairing")
                    links = records(db, "SELECT * FROM local_memory_domains WHERE domain_id=? ORDER BY memory_id,source", (args.target_id,))
                    summary = {"already_merged":True,"applied":False,"target_id":args.target_id,
                        "target_memories_after":len({row["memory_id"] for row in links}),"source_remaining_links":0,
                        "merged_at":redirect["merged_at"]}
                    if args.apply:
                        args.backup_dir.mkdir(parents=True,exist_ok=True)
                        backup_path = args.backup_dir / ("domain-merge-current-"+uuid.uuid4().hex[:12]+".json")
                        audit = {"snapshot_kind":"after_existing_merge","target":dict(target),"redirect":dict(redirect),
                            "links":links,"current_ledger_fingerprint":content_fingerprint(db),"summary":summary}
                        with backup_path.open("x",encoding="utf-8",newline="\n") as stream:
                            json.dump(audit,stream,ensure_ascii=False,indent=2)
                            stream.write("\n"); stream.flush(); os.fsync(stream.fileno())
                        summary["backup"] = str(backup_path.resolve())
                    db.rollback()
                    print(json.dumps(summary,ensure_ascii=True))
                    return
            if source is None or target is None:
                raise RuntimeError("Both active labels must exist; nothing was changed")
            if source["name"].strip().lower() != target["name"].strip().lower():
                raise RuntimeError("Labels do not have the same name; nothing was changed")
            if db.execute("SELECT 1 FROM memory_domain_tombstones WHERE domain_id IN (?,?)", (args.source_id, args.target_id)).fetchone():
                raise RuntimeError("A label is already retired; refresh before merging")
            running = db.execute("SELECT id FROM memory_refinement_runs WHERE json_extract(view_json,'$.status') IN ('analyzing','cancelling')").fetchall()
            if running:
                raise RuntimeError("Stop refinement analysis before merging active label membership")
            # These databases currently carry no such references. Refuse rather
            # than silently guessing how an unknown configuration should map.
            settings = db.execute("SELECT setting_key FROM app_settings WHERE instr(value_json,?)>0", (args.source_id,)).fetchall()
            scope = db.execute("SELECT id FROM memory_layer_documents WHERE instr(scope,?)>0", (args.source_id,)).fetchall()
            if settings or scope:
                raise RuntimeError("Other active references need an explicit migration first")
            links = records(db, "SELECT * FROM local_memory_domains WHERE domain_id IN (?,?) ORDER BY memory_id,domain_id,source", (args.source_id,args.target_id))
            source_ids = {row["memory_id"] for row in links if row["domain_id"] == args.source_id}
            target_ids = {row["memory_id"] for row in links if row["domain_id"] == args.target_id}
            union = source_ids | target_ids
            before = content_fingerprint(db)
            summary = {"source_id":args.source_id,"target_id":args.target_id,"source_memories":len(source_ids),
                "target_memories_before":len(target_ids),"overlap":len(source_ids & target_ids),"expected_union":len(union),
                "ledger_count_before":before[0],"applied":False}
            if not args.apply:
                db.rollback()
                print(json.dumps(summary, ensure_ascii=True))
                return
            now = datetime.datetime.now(datetime.timezone.utc).isoformat()
            selection = "SELECT memory_id FROM local_memory_domains WHERE domain_id IN (?,?)"
            preserved = {}
            for table in ("memory_domain_classification_state", "cloud_domain_retags", "memory_refinement_reviews"):
                preserved[table] = records(db, f"SELECT * FROM {table} WHERE memory_id IN ({selection})", (args.source_id,args.target_id))
            merge_exists = db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_domain_merges'").fetchone()
            prior_merge = records(db, "SELECT * FROM memory_domain_merges WHERE source_id=?", (args.source_id,)) if merge_exists else []
            if prior_merge:
                raise RuntimeError("A merge redirect already exists; refresh before merging")
            backup = {"at":now,"database":str(database.resolve()),"source":dict(source),"target":dict(target),"links":links,
                "preserved":preserved,"merge_table_existed":bool(merge_exists),"prior_merge":prior_merge,
                "ledger_fingerprint":before,"summary":summary}
            args.backup_dir.mkdir(parents=True, exist_ok=True)
            backup_path = args.backup_dir / ("domain-merge-" + datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8] + ".json")
            with backup_path.open("x", encoding="utf-8", newline="\n") as stream:
                json.dump(backup, stream, ensure_ascii=False, indent=2)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            # Extension is safe for v5 readers. The shipped schema migration
            # advances the version only after the new executable is started.
            db.execute("""CREATE TABLE IF NOT EXISTS memory_domain_merges (
                source_id TEXT PRIMARY KEY,target_id TEXT NOT NULL,merged_at TEXT NOT NULL,
                source_json TEXT NOT NULL DEFAULT '{}',CHECK(source_id!=target_id))""")
            db.execute("INSERT INTO memory_domain_merges(source_id,target_id,merged_at,source_json) VALUES (?,?,?,?)",
                (args.source_id,args.target_id,now,json.dumps(dict(source),ensure_ascii=False)))
            db.execute("""INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at)
                SELECT memory_id,?,weight,source,created_at,? FROM local_memory_domains WHERE domain_id=?
                ON CONFLICT(memory_id,domain_id,source) DO UPDATE SET weight=MAX(weight,excluded.weight),updated_at=excluded.updated_at""",
                (args.target_id,now,args.source_id))
            db.execute("DELETE FROM local_memory_domains WHERE domain_id=?", (args.source_id,))
            for memory_id in sorted(source_ids):
                memory = db.execute("SELECT memory,memory_type,durability FROM local_memory_items WHERE id=?", (memory_id,)).fetchone()
                if memory is None:
                    raise RuntimeError("Memory disappeared during write transaction")
                payload = json.dumps(tuple(memory),ensure_ascii=False,separators=(",", ":")).encode("utf-8")
                fingerprint = hashlib.sha256(payload).hexdigest()
                db.execute("""INSERT INTO memory_domain_classification_state(memory_id,content_hash,classified_at) VALUES (?,?,?)
                    ON CONFLICT(memory_id) DO UPDATE SET content_hash=excluded.content_hash,classified_at=excluded.classified_at""", (memory_id,fingerprint,now))
                db.execute("""INSERT INTO cloud_domain_retags(memory_id,classified_at) VALUES (?,?)
                    ON CONFLICT(memory_id) DO UPDATE SET classified_at=excluded.classified_at""", (memory_id,now))
                db.execute("DELETE FROM memory_refinement_reviews WHERE memory_id=?", (memory_id,))
            db.execute("INSERT INTO memory_domain_tombstones(domain_id,deleted_at) VALUES (?,?)", (args.source_id,now))
            db.execute("DELETE FROM memory_domains WHERE id=?", (args.source_id,))
            db.execute("UPDATE memory_domains SET updated_at=? WHERE id=?", (now,args.target_id))
            current = {row[0] for row in db.execute("SELECT DISTINCT memory_id FROM local_memory_domains WHERE domain_id=?", (args.target_id,))}
            if current != union:
                raise RuntimeError("Merged membership differs from the original union; rolled back")
            after = content_fingerprint(db)
            if after != before:
                raise RuntimeError("Memory content changed in this transaction; rolled back")
            if db.execute("SELECT 1 FROM local_memory_domains WHERE domain_id=?", (args.source_id,)).fetchone():
                raise RuntimeError("Retired memberships remain; rolled back")
            if db.execute("PRAGMA foreign_key_check(local_memory_domains)").fetchone():
                raise RuntimeError("Membership foreign-key check failed; rolled back")
            db.commit()
            summary.update({"applied":True,"target_memories_after":len(current),"memory_content_unchanged":True,
                "backup":str(backup_path.resolve()),"cloud_tag_updates":len(source_ids),"database_version_unchanged":True})
            print(json.dumps(summary,ensure_ascii=True))
        except BaseException:
            db.rollback()
            raise


if __name__ == "__main__":
    main()
