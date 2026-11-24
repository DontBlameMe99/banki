use std::fs;
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
/// sync is off.
pub fn prepare_key(ctx: &Context) -> Result<Option<PathBuf>> {
    if !ctx.config_bool("sync_enabled", false) {
        return Ok(None);
    }
    if let Some(key_file) = ctx.config_str("sync_key_file") {
        let trimmed = key_file.trim();
        if !trimmed.is_empty() {
            return Ok(Some(anki::expand_path(trimmed, &plugin_dir(ctx))));
        }
    }
    let key = ctx.config_str("sync_key").unwrap_or_default();
    let path = ctx.data_dir.join(KEY_FILE);
    if fs::read_to_string(&path).is_ok_and(|current| current == key.as_ref()) {
        return Ok(Some(path));
    }
    fs::write(&path, key.as_bytes())?;
    Ok(Some(path))
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
}
