use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use noctalia_rs::{Context, Result};

use crate::anki;
use crate::plugin_dir;

const KEY_FILE: &str = "sync-key";
const LOCK_FILE: &str = "sync.lock";
const SCRIPT: &str = "sync.py";
const PYTHON: &str = "python3";
/// Writes the sync key to the data dir and returns its path. `None` means
/// sync is off or no usable key is configured.
pub fn prepare_key(ctx: &Context) -> Result<Option<PathBuf>> {
    let stored = ctx.data_dir.join(KEY_FILE);
    let configured = configured_key_file(ctx);
    let key_file_is_stored = configured
        .as_ref()
        .is_some_and(|path| same_path(path, &stored));
    let discard_stored = || {
        if !key_file_is_stored {
            remove_secret(&stored);
        }
    };
    if !ctx.config_bool("sync_enabled", false) {
        discard_stored();
        return Ok(None);
    }
    if let Some(key_file) = configured {
        discard_stored();
        return Ok(read_secret(&key_file).map(|_| key_file));
    }
    let key = ctx.config_str("sync_key").unwrap_or_default();
    let trimmed = key.trim().to_owned();
    if trimmed.is_empty() {
        remove_secret(&stored);
        return Ok(None);
    }
    if read_secret(&stored).as_deref() == Some(trimmed.as_str()) {
        tighten_secret(&stored);
        return Ok(Some(stored));
    }
    write_secret(&stored, &trimmed)?;
    Ok(Some(stored))
}

fn configured_key_file(ctx: &Context) -> Option<PathBuf> {
    let key_file = ctx.config_str("sync_key_file")?;
    let trimmed = key_file.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(anki::expand_path(trimmed, &plugin_dir(ctx)))
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(canonical_left), Ok(canonical_right)) => {
            canonical_left == canonical_right
        }
        _ => false,
    }
}

fn read_secret(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

fn write_secret(path: &Path, contents: &str) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let _ = set_private(&file);
    file.write_all(contents.as_bytes())
}

#[cfg(unix)]
fn set_private(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o600);
    file.set_permissions(permissions)
}

#[cfg(not(unix))]
fn set_private(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

fn tighten_secret(path: &Path) {
    if let Ok(file) = fs::File::open(path) {
        let _ = set_private(&file);
    }
}

fn remove_secret(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Syncs the collection before a read. Returns false when the sync failed,
/// so the previous counts stay on the bar.
pub fn sync_if_enabled(ctx: &Context, collection: &Path) -> bool {
    match prepare_key(ctx) {
        Ok(Some(key)) => run(ctx, collection, &key),
        Ok(None) => true,
        Err(_) => false,
    }
}

fn run(ctx: &Context, collection: &Path, key: &Path) -> bool {
    Command::new(PYTHON)
        .arg(plugin_dir(ctx).join(SCRIPT))
        .arg("sync")
        .arg(collection)
        .arg(key)
        .arg(ctx.data_dir.join(LOCK_FILE))
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::process;
    use std::thread;
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::context;

    #[test]
    fn test_prepare_key_keeps_existing_value() {
        let ctx = context(
            json!({ "sync_enabled": true, "sync_key": "secret" }),
            Value::Null,
        );
        let path = prepare_key(&ctx).expect("prepare").expect("key path");
        let first = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .expect("mtime");
        thread::sleep(Duration::from_millis(20));

        let _ = prepare_key(&ctx);

        let second = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .expect("mtime");
        assert_eq!(first, second);
    }

    #[test]
    fn test_prepare_key_skips_when_no_key_is_configured() {
        let ctx = context(json!({ "sync_enabled": true }), Value::Null);

        assert!(prepare_key(&ctx).expect("prepare").is_none());
        assert!(!ctx.data_dir.join(KEY_FILE).exists());
    }

    #[test]
    fn test_prepare_key_skips_missing_or_unreadable_key_file() {
        let missing = env::temp_dir()
            .join(format!("banki-missing-{}", process::id()))
            .join(KEY_FILE);
        let unreadable =
            env::temp_dir().join(format!("banki-keydir-{}", process::id()));
        fs::create_dir_all(&unreadable).expect("key directory");

        for path in [missing, unreadable.clone()] {
            let ctx = context(
                json!({
                    "sync_enabled": true,
                    "sync_key_file": path.to_str().unwrap_or_default(),
                }),
                Value::Null,
            );
            assert!(prepare_key(&ctx).expect("prepare").is_none());
        }
        let _ = fs::remove_dir_all(&unreadable);
    }

    #[cfg(unix)]
    #[test]
    fn test_stored_key_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let ctx = context(
            json!({ "sync_enabled": true, "sync_key": "secret" }),
            Value::Null,
        );
        let stored = ctx.data_dir.join(KEY_FILE);
        fs::write(&stored, "previous").expect("stored key");

        let path = prepare_key(&ctx).expect("prepare").expect("key path");
        let mode =
            fs::metadata(&path).expect("meta").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);
        assert_eq!(fs::read_to_string(&path).expect("read"), "secret");

        let mut permissions = fs::metadata(&path).expect("meta").permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&path, permissions).expect("relaxed");

        prepare_key(&ctx).expect("prepare");

        let mode =
            fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn test_configured_key_file_can_be_the_stored_key() {
        let mut ctx = context(json!({}), Value::Null);
        let stored = ctx.data_dir.join(KEY_FILE);
        fs::write(&stored, "secret").expect("stored key");
        let nested = ctx.data_dir.join("nested");
        fs::create_dir_all(&nested).expect("nested dir");
        let spelling = nested.join("../sync-key");

        ctx.config = json!({
            "sync_enabled": true,
            "sync_key_file": spelling.to_str().unwrap_or_default(),
        });

        let path = prepare_key(&ctx).expect("prepare").expect("key path");

        assert_eq!(path, spelling);
        assert_eq!(fs::read_to_string(&stored).expect("read"), "secret");

        ctx.config = json!({
            "sync_enabled": false,
            "sync_key_file": spelling.to_str().unwrap_or_default(),
        });

        assert!(prepare_key(&ctx).expect("prepare").is_none());
        assert!(stored.exists());
    }

    #[test]
    fn test_external_key_file_replaces_stored_key() {
        let mut ctx = context(
            json!({ "sync_enabled": true, "sync_key": "secret" }),
            Value::Null,
        );
        let stored = prepare_key(&ctx).expect("prepare").expect("key path");
        assert!(stored.exists());
        let external = ctx.data_dir.join("sops-key");
        fs::write(&external, "external").expect("external key");

        ctx.config = json!({
            "sync_enabled": true,
            "sync_key_file": external.to_str().unwrap_or_default(),
        });

        let path = prepare_key(&ctx).expect("prepare").expect("key path");

        assert_eq!(path, external);
        assert!(!stored.exists());
        assert!(external.exists());
    }

    #[test]
    fn test_same_path_resolves_other_spellings() {
        let dir = env::temp_dir().join(format!("banki-path-{}", process::id()));
        let nested = dir.join("nested");
        fs::create_dir_all(&nested).expect("nested dir");
        let key = dir.join(KEY_FILE);
        fs::write(&key, "secret").expect("key");

        assert!(same_path(&key, &nested.join("../sync-key")));
        assert!(same_path(&key, &key));
        assert!(!same_path(&key, &dir.join("other")));
        assert!(!same_path(&key, &dir.join("missing")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sync_if_enabled_removes_stored_key_when_disabled() {
        let mut ctx = context(
            json!({ "sync_enabled": true, "sync_key": "secret" }),
            Value::Null,
        );
        let path = prepare_key(&ctx).expect("prepare").expect("key path");
        assert!(path.exists());

        ctx.config = json!({ "sync_enabled": false, "sync_key": "secret" });

        assert!(sync_if_enabled(&ctx, Path::new("/collection.anki2")));
        assert!(!path.exists());
    }
}
