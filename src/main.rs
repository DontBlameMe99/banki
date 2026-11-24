mod anki;
mod cli;
mod render;
mod sync;
#[cfg(test)]
mod test_support;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use noctalia_rs::{Context, Op, Plugin, Result, unix_seconds};
use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

const TRANSLATIONS: &[(&str, &str)] = &[
    ("settings.collection_path.label", "Collection path"),
    (
        "settings.collection_path.description",
        "Path to collection.anki2 or its profile folder. Leave empty to use the only Anki profile under ~/.local/share/Anki2.",
    ),
    ("settings.refresh_seconds.label", "Refresh interval"),
    (
        "settings.refresh_seconds.description",
        "Seconds between checks, 600 by default. Cards due later still become due between checks.",
    ),
    ("settings.hide_icon.label", "Hide icon"),
    ("settings.hide_text.label", "Hide number"),
    ("settings.hide_on_complete.label", "Hide on complete"),
    ("settings.follow_accent.label", "Follow accent colors"),
    (
        "settings.follow_accent.description",
        "Tints the Anki icon, number, and fill from the palette. Off uses Anki blue.",
    ),
    ("settings.include_new.label", "Include new cards"),
    (
        "settings.include_new.description",
        "Counts every new card. Anki deck limits are not applied, so this is an upper bound.",
    ),
    ("settings.sync_enabled.label", "Sync with AnkiWeb"),
    (
        "settings.sync_enabled.description",
        "Sync the local collection before each check. Needs an AnkiWeb sync key and network access.",
    ),
    ("settings.sync_key.label", "AnkiWeb sync key"),
    (
        "settings.sync_key.description",
        "Private sync key used only when sync is enabled. Stored in Noctalia settings.",
    ),
    ("settings.sync_key_file.label", "AnkiWeb sync key file"),
    (
        "settings.sync_key_file.description",
        "Read the key from this file instead of the setting above. Useful with sops-managed secrets.",
    ),
    ("tooltip.reviews", "Reviews due now"),
    ("tooltip.learning", "Learning due now"),
    ("tooltip.new", "New (no deck limits)"),
    ("tooltip.completed", "Completed today"),
    ("tooltip.total", "Total today"),
    ("tooltip.waiting", "Waiting for Anki data"),
];

/// Last successful count, reused when a read fails.
static DATA: Mutex<Option<anki::Data>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sample") => cli::sample(&args),
        Some("bench") => cli::bench(&args),
        _ => noctalia_rs::run(&plugin_with_translations()),
    }
}

fn plugin_with_translations() -> Plugin {
    TRANSLATIONS
        .iter()
        .fold(plugin(), |plugin, (key, value)| plugin.tr(*key, *value))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the generated handler type returns Result"
)]
fn dispatch(ctx: &Context) -> Result<Vec<Op>> {
    match ctx.event.as_str() {
        "render" | "update" | "refresh" | "resume" => Ok(refresh(ctx)),
        "ipc" if ctx.payload_str("event").as_deref() == Some("refresh") => {
            Ok(refresh(ctx))
        }
        "config_changed" => {
            let _ = sync::prepare_key(ctx);
            Ok(render::ops(ctx, cached().as_ref(), Instant::now()))
        }
        _ => Ok(Vec::new()),
    }
}

fn refresh(ctx: &Context) -> Vec<Op> {
    let started = Instant::now();
    sample(ctx).map_or_else(
        || render::ops(ctx, cached().as_ref(), started),
        |data| {
            store(data.clone());
            render::ops(ctx, Some(&data), started)
        },
    )
}

/// Fresh counts, or `None` when the collection or the sync is unavailable.
fn sample(ctx: &Context) -> Option<anki::Data> {
    let configured = ctx.config_str("collection_path").unwrap_or_default();
    let collection =
        anki::collection_path(&configured, &plugin_dir(ctx)).ok()?;
    if !sync::sync_if_enabled(ctx, &collection) {
        return None;
    }
    let include_new = ctx.config_bool("include_new", false);
    anki::sample(&collection, include_new, unix_seconds()).ok()
}

fn plugin_dir(ctx: &Context) -> PathBuf {
    if let Some(dir) = ctx.host("pluginDir").and_then(Value::as_str) {
        return PathBuf::from(dir);
    }
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

fn cached() -> Option<anki::Data> {
    lock(&DATA).clone()
}

fn store(data: anki::Data) {
    *lock(&DATA) = Some(data);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;

    use serde_json::json;

    use super::*;
    use crate::test_support::context;

    fn due_collection(dir: &Path) -> PathBuf {
        let _ = fs::create_dir_all(dir);
        let path = dir.join("collection.anki2");
        let conn = rusqlite::Connection::open(&path).expect("collection");
        conn.execute_batch(
            "CREATE TABLE col (crt INTEGER);
             CREATE TABLE config (key TEXT PRIMARY KEY, val);
             CREATE TABLE cards (id INTEGER PRIMARY KEY, queue INTEGER, due INTEGER);
             CREATE TABLE revlog (id INTEGER, type INTEGER);
             INSERT INTO col (crt) VALUES (0);
             INSERT INTO config (key, val) VALUES ('schedVer', '1');
             INSERT INTO cards (id, queue, due) VALUES (1, 2, 0), (2, 2, 0), (3, 2, 0);",
        )
        .expect("schema");
        path
    }

    fn rendered_number(ops: &[Op]) -> Option<String> {
        let render =
            ops.iter().find(|op| matches!(op, Op::BarRender { .. }))?;
        let Op::BarRender { tree } = render else {
            return None;
        };
        let value = serde_json::to_value(tree).ok()?;
        let label = value["children"]
            .as_array()?
            .iter()
            .find(|child| child["kind"] == "label")?;
        label["props"]["text"].as_str().map(str::to_owned)
    }

    #[test]
    fn test_refresh_events_read_counts() {
        let dir =
            env::temp_dir().join(format!("banki-cards-{}", process::id()));
        let collection = due_collection(&dir);
        let mut ctx = context(
            json!({ "collection_path": collection.to_str().unwrap_or_default() }),
            json!({ "bar": { "vertical": false } }),
        );

        for event in ["render", "refresh", "resume"] {
            ctx.event = event.to_owned();
            let ops = dispatch(&ctx).expect("dispatch");
            assert_eq!(rendered_number(&ops).as_deref(), Some("3"), "{event}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
