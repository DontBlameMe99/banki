# Banki

## Pull requests with code changes

Any pull request that changes code must show the result. When the change is visible, embed two images in the PR description: a **Before** screenshot and an **After** screenshot. Label both images so reviewers know which is which.

CI treats every change to a source or workflow file as a code change and fails the PR when the description has fewer than two embedded images. Documentation-only PRs are exempt.

Noctalia plugin written in Rust with
[noctalia-rs](https://github.com/lumi673/noctalia-rs). The crate at the
repository root builds the `banki-helper` binary: `src/main.rs` wires the
plugin and its events, `src/render.rs` builds the bar ops, `src/sync.rs` runs
the AnkiWeb sync, `src/cli.rs` holds the `sample` and `bench` subcommands, and
`src/anki.rs` reads the collection. `noctalia.toml` is the source of truth for
metadata, settings, and the widget entry. `banki/` is the generated plugin
directory and is committed, including the static helper binary. `sync.py` is
the only Python code: AnkiWeb sync and sync-key extraction.

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
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
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
