#!/usr/bin/env python3
"""AnkiWeb sync and sync-key extraction for the Banki Rust helper."""

import fcntl
import glob
import io
import os
import pickle
import sqlite3
import sys
from pathlib import Path


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

    path = os.path.expanduser(path)
    if not os.path.isfile(path):
        raise RuntimeError(f"collection not found: {path}")

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



def main():
    if len(sys.argv) < 2:
        print("usage: sync.py sync <collection> <key> <lock> | sync.py key [profile]", file=sys.stderr)
        return 2
    if sys.argv[1] == "key":
        try:
            profile = sys.argv[2] if len(sys.argv) > 2 else None
            print(extract_sync_key(profile))
            return 0
        except Exception as error:
            print(str(error), file=sys.stderr)
            return 1
    if sys.argv[1] != "sync" or len(sys.argv) < 5:
        print("usage: sync.py sync <collection> <key> <lock>", file=sys.stderr)
        return 2
    try:
        sync_collection(sys.argv[2], sys.argv[3], sys.argv[4])
    except Exception as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
