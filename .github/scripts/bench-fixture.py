#!/usr/bin/env python3
"""Create an Anki-shaped collection for `banki-helper bench`."""

import argparse
import random
import sqlite3
import time
from pathlib import Path

SCHEMA = """
CREATE TABLE col (id INTEGER PRIMARY KEY, crt INTEGER NOT NULL, mod INTEGER NOT NULL,
  scm INTEGER NOT NULL, ver INTEGER NOT NULL, dty INTEGER NOT NULL, usn INTEGER NOT NULL,
  ls INTEGER NOT NULL, conf TEXT NOT NULL, models TEXT NOT NULL, decks TEXT NOT NULL,
  dconf TEXT NOT NULL, tags TEXT NOT NULL);
CREATE TABLE config (key TEXT PRIMARY KEY, usn INTEGER NOT NULL, mtime_secs INTEGER NOT NULL, val BLOB NOT NULL);
CREATE TABLE cards (id INTEGER PRIMARY KEY, nid INTEGER NOT NULL, did INTEGER NOT NULL,
  ord INTEGER NOT NULL, mod INTEGER NOT NULL, usn INTEGER NOT NULL, type INTEGER NOT NULL,
  queue INTEGER NOT NULL, due INTEGER NOT NULL, ivl INTEGER NOT NULL, factor INTEGER NOT NULL,
  reps INTEGER NOT NULL, lapses INTEGER NOT NULL, left INTEGER NOT NULL, odue INTEGER NOT NULL,
  odid INTEGER NOT NULL, flags INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE revlog (id INTEGER PRIMARY KEY, cid INTEGER NOT NULL, usn INTEGER NOT NULL,
  ease INTEGER NOT NULL, ivl INTEGER NOT NULL, lastIvl INTEGER NOT NULL, factor INTEGER NOT NULL,
  time INTEGER NOT NULL, type INTEGER NOT NULL);
CREATE INDEX ix_cards_sched ON cards (did, queue, due);
CREATE INDEX ix_cards_nid ON cards (nid);
CREATE INDEX ix_cards_usn ON cards (usn);
CREATE INDEX ix_revlog_cid ON revlog (cid);
CREATE INDEX ix_revlog_usn ON revlog (usn);
"""


def build(path: Path, cards: int, reviews: int, seed: int) -> None:
    rng = random.Random(seed)
    path.parent.mkdir(parents=True, exist_ok=True)
    for suffix in ("", "-wal", "-shm"):
        Path(str(path) + suffix).unlink(missing_ok=True)

    conn = sqlite3.connect(path)
    conn.executescript(SCHEMA)
    now = int(time.time())
    crt = now - 400 * 86_400
    conn.execute(
        "INSERT INTO col VALUES (1, ?, ?, 0, 11, 0, 0, 0, '{}', '{}', '{}', '{}', '{}')",
        (crt, now),
    )
    conn.executemany(
        "INSERT INTO config VALUES (?, 0, 0, ?)",
        [
            ("schedVer", b"2"),
            ("rollover", b"0"),
            ("creationOffset", b"-120"),
        ],
    )

    queues = [2] * (cards * 8 // 10) + [3] * (cards // 10)
    queues += [1] * (cards // 20)
    queues += [0] * (cards - len(queues))
    rng.shuffle(queues)
    day = (now - crt) // 86_400
    conn.executemany(
        "INSERT INTO cards VALUES (?, ?, 1, 0, ?, 0, 2, ?, ?, 10, 2500, 3, 0, 0, 0, 0, 0, '')",
        [
            (
                card,
                card,
                now - rng.randrange(86_400),
                queue,
                day - rng.randrange(200) if queue in (2, 3)
                else now + rng.randrange(-86_400, 86_400),
            )
            for card, queue in enumerate(queues, start=1)
        ],
    )
    base = (now - 86_400) * 1000
    conn.executemany(
        "INSERT INTO revlog VALUES (?, ?, 0, 3, 10, 5, 2500, 1000, ?)",
        [
            (base + index * 10, rng.randrange(1, cards + 1), rng.randrange(4))
            for index in range(reviews)
        ],
    )
    conn.commit()
    conn.execute("PRAGMA journal_mode=WAL")
    conn.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path)
    parser.add_argument("--cards", type=int, default=50_000)
    parser.add_argument("--reviews", type=int, default=500_000)
    parser.add_argument("--seed", type=int, default=11)
    args = parser.parse_args()
    build(args.path, args.cards, args.reviews, args.seed)


if __name__ == "__main__":
    main()
