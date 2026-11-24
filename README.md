# Banki

![Banki](banki/thumbnail.webp)

A Noctalia bar widget that reads due Anki cards from a local collection. It
shows the number learnable right now. Anki does not need to run, and the widget
also works while Anki is open. AnkiWeb sync is optional.

The widget is written in Rust with
[noctalia-rs](https://github.com/lumi673/noctalia-rs). The workspace follows the
usual `crates/lib` plus `crates/bin` split. `crates/lib/noctalia.toml` is the
source of truth. `banki/` holds the generated plugin and the static helper
binary.

## Install

```sh
noctalia msg plugins source add banki git https://github.com/DontBlameMeDev/banki.git
noctalia msg plugins enable dontblameme/banki
```

Then add `dontblameme/banki:banki` to a bar.

See [banki/README.md](banki/README.md) for behavior, settings, and performance.

## Build

```sh
cargo build --release --target x86_64-unknown-linux-musl
python3 .github/scripts/emit.py target/x86_64-unknown-linux-musl/release/banki-helper
cp target/x86_64-unknown-linux-musl/release/banki-helper banki/banki-helper
```

`banki/banki-helper` is a static x86_64 Linux binary. The plugin manager reads
`banki/` directly, so no build step runs on install.

## Development

See [docs/noctalia-plugins.md](docs/noctalia-plugins.md) for the Noctalia plugin
API documentation and [AGENTS.md](AGENTS.md) for the checks.

## Benchmark

Generate a repeatable Anki-shaped collection and time the helper's read path:

```sh
python3 .github/scripts/bench-fixture.py .tmp/collection.anki2
target/x86_64-unknown-linux-musl/release/banki-helper bench .tmp/collection.anki2 1000 1
```

The reported `first` time includes building the in-memory card index. Median
and p95 measure refreshes against an unchanged collection.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

See [docs/upgrading-noctalia-rs.md](docs/upgrading-noctalia-rs.md) for the
dependency and release flow.
