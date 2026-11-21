# Banki

Noctalia plugin with one bar widget entry. The Luau entry and translations are
empty until the widget behavior is defined. There is no build step and no dev
server. The two TOML manifests, `catalog.toml` and `banki/plugin.toml`, must
agree on every shared field.

## Checks

CI runs these:

```sh
python3 .github/scripts/check-catalog.py
python3 /path/to/validate-plugins.py --profile community --root .
```

The plugin validator and Luau LSP come from the pinned action in
`.github/workflows/ci.yml`. `luau` and `luau-lsp` are not installed here, so
rely on CI unless you install them.

## API

`docs/noctalia-plugins.md` links the API pages and `noctalia.d.luau`. Read them
instead of guessing at `noctalia.*`, `ui.*`, or `barWidget.*`.

## Python

PyYAML is not installed. Read `catalog.toml` and `banki/plugin.toml` with
`tomllib`. Do not parse YAML with Python.
