#!/usr/bin/env python3
"""
Migrate AO workflow JSON files to SQLite.

Reads:  ~/.ao/<scope>/workflow-state/*.json + checkpoints/
Writes: ~/.ao/<scope>/workflow.db

Run:    python3 scripts/migrate-workflows-to-sqlite.py
Dry:    python3 scripts/migrate-workflows-to-sqlite.py --dry-run
Single: python3 scripts/migrate-workflows-to-sqlite.py --scope ao-cli-1222ef9c4f94
"""

import argparse
import json
import os
import sqlite3
import sys
import time
from pathlib import Path

AO_ROOT = Path.home() / ".ao"

SCHEMA = """
CREATE TABLE IF NOT EXISTS workflows (
    id              TEXT PRIMARY KEY,
    task_id         TEXT DEFAULT '',
    workflow_ref    TEXT DEFAULT '',
    status          TEXT NOT NULL,
    machine_state   TEXT,
    current_phase   TEXT,
    current_phase_index INTEGER DEFAULT 0,
    subject_title   TEXT,
    subject_desc    TEXT,
    phases_json     TEXT,
    decision_json   TEXT,
    failure_reason  TEXT,
    total_reworks   INTEGER DEFAULT 0,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    checkpoint_count INTEGER DEFAULT 0,
    duration_secs   REAL
);

CREATE INDEX IF NOT EXISTS idx_wf_status     ON workflows(status);
CREATE INDEX IF NOT EXISTS idx_wf_task       ON workflows(task_id) WHERE task_id != '';
CREATE INDEX IF NOT EXISTS idx_wf_ref        ON workflows(workflow_ref);
CREATE INDEX IF NOT EXISTS idx_wf_started    ON workflows(started_at);
CREATE INDEX IF NOT EXISTS idx_wf_completed  ON workflows(completed_at) WHERE completed_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_wf_active     ON workflows(status) WHERE status IN ('running', 'paused');

CREATE TABLE IF NOT EXISTS checkpoints (
    workflow_id   TEXT NOT NULL,
    number        INTEGER NOT NULL,
    timestamp     TEXT NOT NULL,
    reason        TEXT NOT NULL,
    phase_id      TEXT,
    machine_state TEXT,
    status        TEXT,
    PRIMARY KEY (workflow_id, number)
);

CREATE INDEX IF NOT EXISTS idx_cp_workflow ON checkpoints(workflow_id);
"""


def open_db(db_path):
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA foreign_keys=OFF")
    conn.executescript(SCHEMA)
    return conn


def parse_subject(wf):
    subj = wf.get("subject", {})
    for variant in ("Custom", "Task", "Requirement"):
        if variant in subj:
            inner = subj[variant]
            return inner.get("title", ""), inner.get("description", "")
    return "", ""


def compute_duration(wf):
    from datetime import datetime
    started = wf.get("started_at")
    completed = wf.get("completed_at")
    if not started or not completed:
        return None
    try:
        fmt = "%Y-%m-%dT%H:%M:%S"
        s = datetime.fromisoformat(started.replace("Z", "+00:00"))
        c = datetime.fromisoformat(completed.replace("Z", "+00:00"))
        return (c - s).total_seconds()
    except Exception:
        return None


def insert_workflow(conn, wf):
    title, desc = parse_subject(wf)
    conn.execute(
        """INSERT OR REPLACE INTO workflows
           (id, task_id, workflow_ref, status, machine_state,
            current_phase, current_phase_index, subject_title, subject_desc,
            phases_json, decision_json,
            failure_reason, total_reworks, started_at, completed_at,
            checkpoint_count, duration_secs)
           VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (
            wf["id"],
            wf.get("task_id", ""),
            wf.get("workflow_ref", ""),
            wf.get("status", "unknown"),
            wf.get("machine_state"),
            wf.get("current_phase"),
            wf.get("current_phase_index", 0),
            title,
            desc,
            json.dumps(wf.get("phases", []), separators=(",", ":")),
            json.dumps(wf.get("decision_history", []), separators=(",", ":")),
            wf.get("failure_reason"),
            wf.get("total_reworks", 0),
            wf["started_at"],
            wf.get("completed_at"),
            wf.get("checkpoint_metadata", {}).get("checkpoint_count", 0),
            compute_duration(wf),
        ),
    )


def insert_checkpoint_metadata(conn, wf):
    meta = wf.get("checkpoint_metadata", {})
    for cp in meta.get("checkpoints", []):
        conn.execute(
            """INSERT OR REPLACE INTO checkpoints
               (workflow_id, number, timestamp, reason, phase_id,
                machine_state, status)
               VALUES (?,?,?,?,?,?,?)""",
            (
                wf["id"],
                cp["number"],
                cp["timestamp"],
                cp["reason"],
                cp.get("phase_id"),
                cp.get("machine_state"),
                cp.get("status"),
            ),
        )


def migrate_scope(scope_dir, dry_run=False):
    wf_dir = scope_dir / "workflow-state"
    if not wf_dir.exists():
        return None

    scope_name = scope_dir.name
    db_path = scope_dir / "workflow.db"

    json_files = [
        f for f in wf_dir.iterdir()
        if f.suffix == ".json" and f.name != "_active_index.json"
    ]
    if not json_files:
        return None

    cp_dir = wf_dir / "checkpoints"
    cp_count = 0
    if cp_dir.exists():
        for wf_cp_dir in cp_dir.iterdir():
            if wf_cp_dir.is_dir():
                cp_count += sum(1 for f in wf_cp_dir.iterdir() if f.suffix == ".json")

    if dry_run:
        total_bytes = sum(f.stat().st_size for f in json_files)
        return {
            "scope": scope_name,
            "workflows": len(json_files),
            "checkpoints": cp_count,
            "json_bytes": total_bytes,
            "db_path": str(db_path),
        }

    conn = open_db(db_path)
    migrated = 0
    errors = 0
    cp_total = 0

    for f in json_files:
        try:
            raw = f.read_text()
            wf = json.loads(raw)
            insert_workflow(conn, wf)
            insert_checkpoint_metadata(conn, wf)
            cp_total += len(wf.get("checkpoint_metadata", {}).get("checkpoints", []))
            migrated += 1
        except Exception as e:
            errors += 1
            print(f"  WARN: {f.name}: {e}", file=sys.stderr)

    conn.commit()
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")

    db_size = db_path.stat().st_size
    conn.close()

    return {
        "scope": scope_name,
        "workflows": migrated,
        "checkpoints": cp_total,
        "errors": errors,
        "db_path": str(db_path),
        "db_bytes": db_size,
    }


def main():
    parser = argparse.ArgumentParser(description="Migrate AO workflow JSON to SQLite")
    parser.add_argument("--dry-run", action="store_true", help="Show what would be migrated")
    parser.add_argument("--scope", help="Only migrate a specific scope directory name")
    parser.add_argument("--delete-json", action="store_true", help="Delete JSON files after successful migration")
    args = parser.parse_args()

    if not AO_ROOT.exists():
        print(f"No AO root found at {AO_ROOT}")
        sys.exit(1)

    scopes = []
    for entry in sorted(AO_ROOT.iterdir()):
        if not entry.is_dir():
            continue
        if args.scope and entry.name != args.scope:
            continue
        if (entry / "workflow-state").exists():
            scopes.append(entry)

    if not scopes:
        print("No workflow-state directories found.")
        sys.exit(0)

    print(f"{'[DRY RUN] ' if args.dry_run else ''}Migrating {len(scopes)} project(s)...\n")

    total_wf = 0
    total_cp = 0
    total_json_bytes = 0
    total_db_bytes = 0
    t0 = time.time()

    for scope_dir in scopes:
        result = migrate_scope(scope_dir, dry_run=args.dry_run)
        if result is None:
            continue

        wf = result["workflows"]
        cp = result.get("checkpoints", 0)
        total_wf += wf
        total_cp += cp

        if args.dry_run:
            jb = result["json_bytes"]
            total_json_bytes += jb
            print(f"  {result['scope']}: {wf} workflows, {cp} checkpoints, {jb/1024/1024:.1f} MB JSON")
        else:
            db = result["db_bytes"]
            total_db_bytes += db
            errs = result.get("errors", 0)
            err_str = f" ({errs} errors)" if errs else ""
            print(f"  {result['scope']}: {wf} workflows, {cp} checkpoints -> {db/1024/1024:.1f} MB db{err_str}")

            if args.delete_json and result.get("errors", 0) == 0:
                wf_dir = scope_dir / "workflow-state"
                deleted = 0
                for f in wf_dir.iterdir():
                    if f.suffix == ".json" and f.name != "_active_index.json":
                        f.unlink()
                        deleted += 1
                cp_dir = wf_dir / "checkpoints"
                if cp_dir.exists():
                    import shutil
                    shutil.rmtree(cp_dir)
                idx = wf_dir / "_active_index.json"
                if idx.exists():
                    idx.unlink()
                print(f"    cleaned up {deleted} JSON files + checkpoints/")

    elapsed = time.time() - t0
    print(f"\nDone in {elapsed:.1f}s — {total_wf} workflows, {total_cp} checkpoints")
    if args.dry_run:
        print(f"Total JSON on disk: {total_json_bytes/1024/1024:.0f} MB")
    else:
        print(f"Total DB size: {total_db_bytes/1024/1024:.0f} MB")


if __name__ == "__main__":
    main()
