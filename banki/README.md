# Banki

Banki is a Noctalia bar widget that shows how many local Anki cards are
learnable right now. It reads `collection.anki2` through a read-only SQLite
connection in a small Rust helper. Anki does not need to run, and it works
while Anki is open. By default, no AnkiWeb account is used.

![Banki](thumbnail.webp)

## Plugin

| Field | Value |
| --- | --- |
| ID | `dontblameme/banki` |
| Entries | Bar widget: `banki` |
| Plugin API | 56 or newer |

## Requirements

Counting needs no runtime beyond the bundled static helper. The helper is a
static x86-64 Linux binary. Other architectures need a native build. Anki is
only needed to create the collection. Optional sync uses Python 3, Anki's
Python package, and AnkiWeb.

## Usage

Add the `dontblameme/banki:banki` widget to a bar. The number counts reviews and
learning cards whose due time has passed. Reviews and day-learning cards use
Anki's day number, and intraday learning cards use a Unix timestamp. Suspended
and buried cards are ignored. Cards due later are ignored even when Anki's own
learn-ahead window would show them.

The widget counts all decks, not only the deck currently open in Anki. With the
v2 scheduler, day boundaries follow the collection's rollover hour, creation
offset, and daylight saving changes. The v1 scheduler uses fixed 24-hour
periods from the collection creation time. The helper opens
the database read-only and never writes to it. While Anki holds its exclusive
lock, the helper reads a temporary copy of the collection and its write-ahead
log, so refreshes keep working and include reviews from the open session.

Completed work is the number of revlog answer events since the Anki day started,
excluding manual and rescheduled entries. Total today is completed work plus the
remaining count.

## Settings

Collection path, refresh interval, and include new cards are plugin settings
under Settings → Plugins → Banki. Icon, number, completion visibility, and
accent colors are bar-widget settings.

Collection path selects `collection.anki2` or its profile folder. Leave it empty
to use the only profile under `~/.local/share/Anki2`. Refresh interval defaults
to 10 minutes. The icon, number, and whole widget on completion can each be
hidden. A hidden widget keeps its update interval, so the next due card makes it
appear again. Follow accent colors tints the icon and number from the palette;
turning it off uses Anki blue. The completion check stays green. Include new
cards is off by default because a raw new-card count ignores deck limits and is
only an upper bound.

## AnkiWeb sync

Sync is off by default. When enabled, the helper syncs the local collection
before each check, so changes made on another device appear without opening
Anki. It needs network access, Python 3, and an AnkiWeb sync key. Put the key in
the sync key setting, or point the key file setting at a secret managed by sops.
The key file takes precedence. A full download is accepted automatically. A full
upload requires a decision in Anki; the widget keeps showing the last known
count until a check succeeds.

The sync controls are plugin settings under Settings → Plugins → Banki, not
bar-widget settings. Multiple widget instances share a lock file, so only one
syncs at a time.

If Anki on this machine has ever been logged in, the key can be copied from its
profile prefs. Run this from the plugin directory:

```sh
python3 sync.py key
python3 sync.py key main
```

The first form uses the last loaded profile. The second selects a profile by
name. The key file and `/run/secrets/anki/sync-key` take precedence, so the
profile name is used only when neither supplies a key. The command prints only
the key, so it can be redirected into a sops-managed file.

On NixOS with the Anki home module, the key is usually already at
`/run/secrets/anki/sync-key`. The `key` command checks that file first, then the
profile prefs.

When Anki is open, the widget skips its sync step and lets Anki manage syncing.
It still refreshes counts from the open collection.

The tooltip lists reviews due now, learning due now, completed today, and total
today. Left click opens Anki and right click refreshes now.

## Performance

One Rust helper process stays alive per widget instance. The helper queries
SQLite only when a refresh is due. Noctalia starts it through the noctalia-rs
bridge, which waits on filesystem events. An idle instance uses about 0.00% of
one core.

The helper opens a read-only connection for a cold refresh, then keeps card due
times and the completed count in memory. It rebuilds them when the collection
changes. An unchanged refresh on a 50,000-card fixture takes about 0.003 ms,
down from 13.6 ms before the cache. The first refresh after startup or after an
Anki write reads the collection once. The last good value stays visible if a
later check fails.

Sync runs in a short-lived Python process only when enabled, so Anki's package
is not kept resident between checks.

## Icon

The bundled icon uses the Anki path from Simple Icons, released under CC0 1.0,
in Anki's brand blue. The name Anki and its logo belong to their respective
owners.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
