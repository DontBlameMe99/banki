# Upgrading noctalia-rs

Banki pins `noctalia-rs` from crates.io. A merge on upstream `main` is not
enough. Wait for a published crate version.

## Run the bump

1. Open **Actions → Bump noctalia-rs → Run workflow** with the released
   version, for example `0.2.1`. For the pull request to trigger CI
   automatically, set a `BANKI_TOKEN` repository secret with fine-grained
   `Contents: read and write` and `Pull requests: read and write`. Without it,
   the workflow uses `GITHUB_TOKEN`, and GitHub does not start a second
   workflow run for the PR. In that case, run the CI workflow manually on the
   bump branch.
2. The workflow updates `Cargo.toml` and `Cargo.lock`, rebuilds
   `banki/banki-helper` for x86_64 musl, regenerates `banki/`, runs the Rust
   checks and the catalog check, and opens a pull request.
3. Review the diff. In particular:
   - `banki/plugin.toml` still says `plugin_api = 56`. Noctalia 5.6.0 supports
     APIs 3 through 56. Do not raise it until the installed shell does.
   - `emit.py` still needs its compatibility edits. Remove a patch only when a
     fresh emit no longer needs it.
   - If the helper's idle behavior changed, update the performance section in
     `banki/README.md` and replace the old measurements.
4. Merge the pull request.

## Release Banki

Run **Actions → Release** with `minor` for a feature, or `patch` for a
dependency-only update. The release workflow bumps `Cargo.toml` and
`catalog.toml`, rebuilds the static helper, regenerates `banki/`, opens and
merges a version-bump pull request, and creates the GitHub release. The release
workflow requires a `BANKI_TOKEN` repository secret with fine-grained
`Contents: read and write` and `Pull requests: read and write`. The main ruleset
cannot merge a pull request whose changes come from the Actions app, and a PAT
push also makes the pull request checks run.

## Deploy on NixOS

The NixOS flake consumes the repository directly:

```nix
banki = {
  url = "github:DontBlameMeDev/banki";
  flake = false;
};
```

The Noctalia module uses the store path of that input:

```nix
{
  name = "banki";
  kind = "path";
  location = "${inputs.banki}";
  enabled = true;
}
```

After the release commit is on `main`:

```sh
cd ~/.config/nixos
nix flake update banki
```

Then rebuild with the usual generation script. The new store path is written
into the Noctalia configuration. The plugin data directory keeps the sync key,
the tinted icon, and the sync lock, so no migration is needed.

Verify:

```sh
noctalia msg plugins list | grep banki
ps -o pid,rss,pcpu,args -C banki-helper
```

The plugin version should match the release. The helper should sit near 0.00%
CPU while idle.

If you want the flake input pinned to a release tag instead of `main`, use
`github:DontBlameMeDev/banki/vX.Y.Z` and update the lock.

## If the crate is not released yet

Wait. A git dependency pinned to the merge commit works for testing, but it
should not ship. Switch back to the crates.io release before the Banki release.
