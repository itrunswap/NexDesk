"""Create online SQLite snapshots and recover a stopped NexDesk installation."""

import argparse
import os
import sqlite3
import tempfile
import time
from contextlib import closing
from pathlib import Path

TABLES = {
    "desktop": {"lan_recent", "lan_listener"},
    "directory": {"device"},
}


def connection(path, readonly=False):
    uri = path.resolve().as_uri()
    db = sqlite3.connect(uri + ("?mode=ro" if readonly else "?mode=rw"), uri=True, timeout=4)
    db.execute("PRAGMA trusted_schema=OFF")
    return db


def check(db, kind):
    if db.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
        raise ValueError("SQLite integrity check failed")
    tables = {name for (name,) in db.execute("SELECT name FROM sqlite_schema WHERE type='table'")}
    if not TABLES[kind] <= tables or TABLES["desktop" if kind == "directory" else "directory"] <= tables:
        raise ValueError("SQLite database does not match the selected NexDesk component")
    return tables


def copy_database(source, destination):
    last_progress = time.monotonic()

    def progress(status, _remaining, _total):
        nonlocal last_progress
        now = time.monotonic()
        if status in (sqlite3.SQLITE_BUSY, sqlite3.SQLITE_LOCKED):
            if now - last_progress > 5:
                raise sqlite3.OperationalError("database remained busy")
        else:
            last_progress = now

    source.backup(destination, pages=512, progress=progress, sleep=0.05)


def create_empty(path):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)


def publish(path, target):
    with path.open("r+b") as snapshot:
        os.fsync(snapshot.fileno())
    os.link(path, target)


def backup(source, target, kind):
    with tempfile.TemporaryDirectory(prefix=".nexdesk-backup-", dir=target.parent) as scratch:
        stage = Path(scratch, "snapshot.sqlite3")
        create_empty(stage)
        with closing(connection(source, readonly=True)) as original, closing(connection(stage)) as snapshot:
            copy_database(original, snapshot)
            check(snapshot, kind)
        publish(stage, target)


def preserve_revocations(snapshot, existing):
    revoked = existing.execute(
        "SELECT id, registration_key, fingerprint FROM device WHERE revoked=1"
    ).fetchall()
    for device, registration, fingerprint in revoked:
        saved = snapshot.execute(
            "SELECT id, registration_key, fingerprint FROM device WHERE id=? OR registration_key=?",
            (device, registration),
        ).fetchall()
        identity = (device, registration, fingerprint)
        if not saved:
            snapshot.execute("INSERT INTO device VALUES (?,?,?,1)", identity)
        elif saved == [identity]:
            snapshot.execute("UPDATE device SET revoked=1 WHERE id=?", (device,))
        else:
            raise ValueError("revoked device belongs to a different directory database")


def restore(source, target, before, kind):
    existing_target = target.exists()
    if source == target or (existing_target and source.samefile(target)):
        raise ValueError("source and destination must be different files")
    if existing_target and before is None:
        raise ValueError("an existing database requires a new --before snapshot path")
    if before is not None and (before in (source, target) or before.exists()):
        raise ValueError("--before must be a new path distinct from source and destination")
    with tempfile.TemporaryDirectory(prefix=".nexdesk-recovery-", dir=target.parent) as scratch:
        stage = Path(scratch, "restored.sqlite3")
        create_empty(stage)
        with closing(connection(source, readonly=True)) as saved, closing(connection(stage)) as snapshot:
            copy_database(saved, snapshot)
            tables = check(snapshot, kind)
            with snapshot:
                if kind == "desktop":
                    # Restoring an older snapshot must not reactivate revoked approvals.
                    if "trusted_controller" in tables:
                        snapshot.execute("DELETE FROM trusted_controller")
                    if "desktop_access_policy" in tables:
                        snapshot.execute("UPDATE desktop_access_policy SET unattended=0")
                elif existing_target:
                    with closing(connection(target, readonly=True)) as existing:
                        check(existing, kind)
                        preserve_revocations(snapshot, existing)
            check(snapshot, kind)
            if target.exists() != existing_target:
                raise ValueError("destination changed during recovery")
            if existing_target:
                backup(target, before, kind)
                with closing(connection(target)) as live:
                    copy_database(snapshot, live)
                    check(live, kind)
                return
        publish(stage, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("backup", "restore"):
        action = commands.add_parser(command)
        action.add_argument("source", type=Path)
        action.add_argument("target", type=Path)
        action.add_argument("--kind", choices=TABLES, required=True)
        if command == "restore":
            action.add_argument("--before", type=Path)
            action.add_argument("--stopped", action="store_true", required=True)
    args = parser.parse_args()
    source = args.source.resolve()
    target = args.target.resolve()
    try:
        if args.command == "backup":
            backup(source, target, args.kind)
        else:
            restore(source, target, args.before.resolve() if args.before else None, args.kind)
    except (OSError, ValueError, sqlite3.Error) as error:
        parser.exit(1, f"{error}\n")
    print("SQLite snapshot verified")


if __name__ == "__main__":
    main()
