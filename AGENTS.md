# Banki

Noctalia plugin written in Rust with
[noctalia-rs](https://github.com/lumi673/noctalia-rs). The workspace follows
`~/projects/project_templates/rust`: `crates/lib` holds the plugin definition,
rendering, and Anki logic, and `crates/bin` is the helper CLI.
`crates/lib/noctalia.toml` is the source of truth for metadata, settings, and
the widget entry. `banki/` is the generated plugin directory and is committed,
including the static helper binary. `crates/lib/sync.py` is the only Python
code: AnkiWeb sync and sync-key extraction.

Do not edit generated files in `banki/` by hand. Run:

```sh
cargo build --release --target x86_64-unknown-linux-musl
python3 .github/scripts/emit.py target/x86_64-unknown-linux-musl/release/banki-helper
cp target/x86_64-unknown-linux-musl/release/banki-helper banki/banki-helper
```

Keep the prebuilt helper static. `cargo build --target x86_64-unknown-linux-musl`
needs the musl toolchain and target; CI and the release workflow install them.
Renaming or replacing a running helper needs an atomic move, not an in-place
copy.

## Checks

CI runs these:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 .github/scripts/check-catalog.py
python3 /path/to/validate-plugins.py --profile community --root .
luau-lsp analyze --definitions=/path/to/noctalia.d.luau --ignore 'banki/lib/bridge.luau' banki
```

`emit.py` runs the library emitter, then adds the old `--!nolint FunctionUnused`
header and drops the retired color roles the validator rejects from the generated
bridge. Keep those edits in that script, not in `banki/`.

The plugin validator and Luau LSP come from the pinned action in
`.github/workflows/ci.yml`. `banki/` must match a fresh `emit`; CI regenerates
it and fails on a diff. `luau` and `luau-lsp` are not installed here, so rely on
CI unless you install them.

## API

`docs/noctalia-plugins.md` links the API pages and `noctalia.d.luau`. Read them
instead of guessing at `noctalia.*`, `ui.*`, or `barWidget.*`. noctalia-rs also
ships the generated bridge and typed ops used by `src/main.rs`.

## Python

PyYAML is not installed. Read `catalog.toml` and `banki/plugin.toml` with
`tomllib`. Do not parse YAML with Python. Do not move the count path back into
Python: Anki's package is needed only for sync.

## Dependency updates

Run the "Bump noctalia-rs" workflow with the released crate version. It updates
`Cargo.toml` and `Cargo.lock`, rebuilds the static helper, regenerates `banki/`,
runs the checks, and opens a PR. See
[docs/upgrading-noctalia-rs.md](docs/upgrading-noctalia-rs.md) for the release
and NixOS deployment steps.
