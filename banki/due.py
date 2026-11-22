#!/usr/bin/env python3
"""Emit Anki due counts as JSON lines for the Banki bar widget."""

import contextlib
import glob
import io
import fcntl
import json
import os
import pickle
import shutil
import sqlite3
import sys
import tempfile
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path


def config(con, key):
    row = con.execute("SELECT val FROM config WHERE key = ?", (key,)).fetchone()
    if row is None:
        return None
    value = row[0]
    return value.decode() if isinstance(value, bytes) else str(value)


def fixed_offset(minutes_west):
    return timezone(timedelta(minutes=-minutes_west))


def rollover_stamp(timestamp, minutes_west, hour):
    local = datetime.fromtimestamp(timestamp, fixed_offset(minutes_west))
    local = local.replace(hour=hour % 24, minute=0, second=0, microsecond=0)
    return int(local.timestamp())


def timing(con, now):
    crt = int(con.execute("SELECT crt FROM col").fetchone()[0])
    current_west = -time.localtime(now).tm_gmtoff // 60
    raw_rollover = config(con, "rollover")
    rollover = int(raw_rollover) if raw_rollover not in (None, "") else 4

    if config(con, "schedVer") == "1":
        days = (now - crt) // 86400
        return days, crt + (days + 1) * 86400

    created_west = config(con, "creationOffset")
    next_day = None
    if created_west is None:
        crt_rollover = rollover_stamp(crt, current_west, rollover)
        days = (now - crt_rollover) // 86400
    else:
        created = datetime.fromtimestamp(crt, fixed_offset(int(created_west)))
        local = datetime.fromtimestamp(now, fixed_offset(current_west))
        today_rollover = local.replace(hour=rollover % 24, minute=0, second=0, microsecond=0)
        rollover_passed = today_rollover <= local
        days = local.date().toordinal() - created.date().toordinal()
        if not rollover_passed:
            days -= 1
        days = max(days, 0)
        next_day = int(today_rollover.timestamp())
        if rollover_passed:
            next_day += 86400

    if next_day is None:
        next_day = rollover_stamp(now, current_west, rollover)
        if next_day < now:
            next_day += 86400
    return days, next_day


def collection_path(raw):
    path = os.path.expanduser((raw or "").strip())
    if path:
        if os.path.isdir(path):
            path = os.path.join(path, "collection.anki2")
        return path

    matches = sorted(glob.glob(os.path.expanduser("~/.local/share/Anki2/*/collection.anki2")))
    if not matches:
        raise RuntimeError("no Anki collection found")
    if len(matches) > 1:
        raise RuntimeError("multiple Anki profiles; set collection_path")
    return matches[0]


def anki_site_packages():
    override = os.environ.get("ANKI_SITE_PACKAGES")
    if override:
        return override
    patterns = (
        "/nix/store/*-anki-*-lib/lib/python*/site-packages",
        "/usr/lib/python*/site-packages",
    )
    for pattern in patterns:
        for candidate in sorted(glob.glob(pattern), reverse=True):
            if os.path.isdir(os.path.join(candidate, "anki")):
                return candidate
    return None


def load_anki():
    site = anki_site_packages()
    if not site:
        raise RuntimeError("Anki Python package not found; set ANKI_SITE_PACKAGES")
    if site not in sys.path:
        sys.path.insert(0, site)
    from anki.collection import Collection
    from anki.sync import SyncAuth

    return Collection, SyncAuth


def sync_collection(path, key_path, lock_path=""):
    try:
        with open(os.path.expanduser(key_path), encoding="utf-8") as handle:
            key = handle.read().strip()
    except OSError as error:
        raise RuntimeError(f"cannot read sync key: {error}") from error
    if not key:
        raise RuntimeError("sync enabled without a key")

    lock = None
    if lock_path:
        lock = open(os.path.expanduser(lock_path), "w")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            lock.close()
            return False

    try:
        Collection, SyncAuth = load_anki()
        try:
            col = Collection(path)
            try:
                auth = SyncAuth(hkey=key)
                out = col.sync_collection(auth, False)
                if out.required == 3:
                    col.full_upload_or_download(auth=auth, server_usn=None, upload=False)
                elif out.required in (2, 4):
                    raise RuntimeError("AnkiWeb requires a full sync decision; resolve it in Anki")
            finally:
                col.close()
        except Exception as error:
            message = str(error).lower()
            if "locked" in message or "already open" in message:
                return False
            raise
    finally:
        if lock:
            fcntl.flock(lock, fcntl.LOCK_UN)
            lock.close()
    return True




class _PrefStub:
    def __init__(self, *args, **kwargs):
        pass

    def __setstate__(self, state):
        self.__dict__.update(state)


class _PrefUnpickler(pickle.Unpickler):
    def find_class(self, module, name):
        return _PrefStub


def extract_sync_key(profile=None):
    candidates = [os.environ.get("ANKI_SYNC_KEY_FILE"), "/run/secrets/anki/sync-key"]
    for candidate in candidates:
        if not candidate:
            continue
        path = os.path.expanduser(candidate)
        if not os.path.isfile(path):
            continue
        with open(path, encoding="utf-8") as handle:
            key = handle.read().strip()
        if key:
            return key
    base = os.environ.get("ANKI_BASE", "~/.local/share/Anki2")
    path = os.path.join(os.path.expanduser(base), "prefs21.db")
    if not os.path.isfile(path):
        raise RuntimeError(f"no Anki prefs database at {path}")

    uri = Path(path).absolute().as_uri() + "?mode=ro"
    con = sqlite3.connect(uri, uri=True, timeout=1)
    try:
        rows = con.execute("SELECT name, data FROM profiles").fetchall()
    finally:
        con.close()

    prefs = {}
    for name, data in rows:
        prefs[name] = _PrefUnpickler(io.BytesIO(data)).load()

    if profile is None:
        profile = prefs.get("_global", {}).get("last_loaded_profile_name")
    if profile:
        if profile not in prefs:
            raise RuntimeError(f"unknown Anki profile {profile}")
        key = prefs[profile].get("syncKey")
        if not key:
            raise RuntimeError(f"no sync key stored for profile {profile}")
        return key

    candidates = [
        (name, value.get("syncKey"))
        for name, value in prefs.items()
        if name != "_global" and value.get("syncKey")
    ]
    if not candidates:
        raise RuntimeError("no sync key stored in Anki prefs")
    if len(candidates) > 1:
        raise RuntimeError("multiple profiles have sync keys; pass a profile name")
    return candidates[0][1]


def open_readonly(path):
    uri = Path(path).absolute().as_uri() + "?mode=ro"
    con = sqlite3.connect(uri, uri=True, timeout=1)
    try:
        con.execute("select 1 from col limit 1").fetchone()
    except sqlite3.OperationalError:
        con.close()
        raise
    return con


@contextlib.contextmanager
def collection_connection(path):
    try:
        con = open_readonly(path)
    except sqlite3.OperationalError as error:
        if "locked" not in str(error).lower():
            raise
        with tempfile.TemporaryDirectory(prefix="banki-") as directory:
            target = Path(directory) / Path(path).name
            for suffix in ("", "-wal"):
                source = Path(str(path) + suffix)
                if source.exists():
                    shutil.copyfile(source, str(target) + suffix)
            con = sqlite3.connect(target, timeout=1)
            try:
                yield con
            finally:
                con.close()
        return
    try:
        yield con
    finally:
        con.close()


def sample(path, include_new, now):
    with collection_connection(path) as con:
        days, next_day = timing(con, now)
        reviews, day_learning, learning, new = con.execute(
            """
            SELECT
              coalesce(sum(CASE WHEN queue = 2 AND due <= ? THEN 1 ELSE 0 END), 0),
              coalesce(sum(CASE WHEN queue = 3 AND due <= ? THEN 1 ELSE 0 END), 0),
              coalesce(sum(CASE WHEN queue = 1 AND due <= ? THEN 1 ELSE 0 END), 0),
              coalesce(sum(CASE WHEN queue = 0 THEN 1 ELSE 0 END), 0)
            FROM cards WHERE queue >= 0
            """,
            (days, days, now),
        ).fetchone()
        completed = con.execute(
            "SELECT count(*) FROM revlog WHERE id >= ? AND type BETWEEN 0 AND 3",
            ((next_day - 86400) * 1000,),
        ).fetchone()[0]

    if not include_new:
        new = 0
    learning += day_learning
    remaining = reviews + learning + new
    return {
        "reviews": reviews,
        "learning": learning,
        "new": new,
        "remaining": remaining,
        "completed": completed,
        "total": remaining + completed,
        "at": now,
    }


def main():
    if len(sys.argv) < 2:
        print(json.dumps({"ok": False, "error": "missing collection path"}), flush=True)
        return 2
    if sys.argv[1] == "key":
        try:
            profile = sys.argv[2] if len(sys.argv) > 2 else None
            print(extract_sync_key(profile))
            return 0
        except Exception as error:
            print(str(error), file=sys.stderr)
            return 1

    raw_path = sys.argv[1]
    raw_mode = sys.argv[2] if len(sys.argv) > 2 else "600"
    include_new = len(sys.argv) > 3 and sys.argv[3] == "1"
    key_path = sys.argv[4] if len(sys.argv) > 4 else ""
    lock_path = sys.argv[5] if len(sys.argv) > 5 else ""
    once = raw_mode == "once"

    try:
        interval = 600 if raw_mode == "once" else max(5, int(raw_mode))
    except ValueError:
        interval = 600

    try:
        path = collection_path(raw_path)
    except Exception as error:
        print(json.dumps({"ok": False, "error": str(error)}), flush=True)
        return 1

    while True:
        now = int(time.time())
        try:
            if key_path:
                sync_collection(path, key_path, lock_path)
            result = sample(path, include_new, now)
            result["ok"] = True
            print(json.dumps(result, separators=(",", ":")), flush=True)
        except Exception as error:
            print(json.dumps({"ok": False, "error": str(error), "at": now}), flush=True)
        if once:
            return 0
        time.sleep(max(1, interval - (int(time.time()) - now)))


if __name__ == "__main__":
    sys.exit(main())
