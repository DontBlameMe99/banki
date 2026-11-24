use std::env;
use std::fs;
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};

use noctalia_rs::{Context, Parts};
use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn translations() -> Value {
    json!({
        "tooltip": {
            "reviews": "Reviews due now",
            "learning": "Learning due now",
            "new": "New (no deck limits)",
            "completed": "Completed today",
            "total": "Total today",
            "waiting": "Waiting for Anki data"
        }
    })
}

/// A render event with an empty state and its own data dir.
pub fn context(config: Value, host: Value) -> Context {
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        env::temp_dir().join(format!("banki-test-{}-{unique}", process::id()));
    let _ = fs::create_dir_all(&dir);
    Context::new(
        Parts {
            entry: "banki".to_owned(),
            event: "render".to_owned(),
            payload: Value::Null,
            config,
            state: json!({}),
            host,
            data_dir: dir,
        },
        translations(),
    )
}
