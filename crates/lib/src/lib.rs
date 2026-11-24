mod anki;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use noctalia_rs::ui::UiNode;
use noctalia_rs::{Context, Op, Plugin, Result, ui};
use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/generated.rs"));

const ICON_SOURCE: &str = "assets/anki.svg";
const ICON_FILL: &str = "#80C2EE";
const SUCCESS: &str = "#4CAF50";

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

static DATA: Mutex<Option<anki::Data>> = Mutex::new(None);
static ICON: Mutex<Option<(String, String)>> = Mutex::new(None);

fn banki_plugin() -> Plugin {
    let mut plugin = plugin();
    for (key, value) in TRANSLATIONS {
        plugin = plugin.tr(*key, *value);
    }
    plugin
}

pub fn run() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sample") => command_sample(&args),
        Some("bench") => command_bench(&args),
        _ => noctalia_rs::run(&banki_plugin()),
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the generated handler type returns Result"
)]
fn dispatch(ctx: &Context) -> Result<Vec<Op>> {
    match ctx.event.as_str() {
        "render" | "update" => Ok(refresh(ctx)),
        "ipc" => {
            if ctx.payload_str("event").as_deref() == Some("refresh") {
                Ok(refresh(ctx))
            } else {
                Ok(Vec::new())
            }
        }
        "config_changed" => {
            let _ = prepare_key(ctx);
            Ok(render_ops(ctx, cached().as_ref(), Instant::now()))
        }
        _ => Ok(Vec::new()),
    }
}

fn refresh(ctx: &Context) -> Vec<Op> {
    let start = Instant::now();
    let plugin_dir = plugin_dir(ctx);
    let configured = ctx.config_str("collection_path").unwrap_or_default();
    let Ok(collection) = anki::collection_path(&configured, &plugin_dir) else {
        return render_ops(ctx, cached().as_ref(), start);
    };

    if !prepare_and_sync(ctx, &collection) {
        return render_ops(ctx, cached().as_ref(), start);
    }

    anki::sample(
        &collection,
        ctx.config_bool("include_new", false),
        anki::now_seconds(),
    )
    .map_or_else(
        |_| render_ops(ctx, cached().as_ref(), start),
        |data| {
            set_cached(Some(data.clone()));
            render_ops(ctx, Some(&data), start)
        },
    )
}

fn prepare_and_sync(ctx: &Context, collection: &Path) -> bool {
    if !ctx.config_bool("sync_enabled", false) {
        return true;
    }
    match prepare_key(ctx) {
        Ok(Some(key)) => run_sync(ctx, collection, &key),
        Ok(None) => true,
        Err(_) => false,
    }
}

fn prepare_key(ctx: &Context) -> Result<Option<PathBuf>> {
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
    let path = ctx.data_dir.join("sync-key");
    if fs::read_to_string(&path).is_ok_and(|current| current == key.as_ref()) {
        return Ok(Some(path));
    }
    fs::write(&path, key.as_bytes())?;
    Ok(Some(path))
}

fn run_sync(ctx: &Context, collection: &Path, key: &Path) -> bool {
    let script = plugin_dir(ctx).join("sync.py");
    let lock = ctx.data_dir.join("sync.lock");
    let Ok(status) = Command::new("python3")
        .arg(script)
        .arg("sync")
        .arg(collection)
        .arg(key)
        .arg(lock)
        .stdout(Stdio::null())
        .status()
    else {
        return false;
    };
    status.success()
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
    DATA.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn set_cached(data: Option<anki::Data>) {
    *DATA.lock().unwrap_or_else(PoisonError::into_inner) = data;
}

fn render_ops(
    ctx: &Context,
    maybe_data: Option<&anki::Data>,
    start: Instant,
) -> Vec<Op> {
    let follow_accent = ctx.config_bool("follow_accent", true);
    let number = if follow_accent { "primary" } else { ICON_FILL };
    let primary = if follow_accent {
        ctx.color("primary")
            .and_then(Value::as_str)
            .unwrap_or(ICON_FILL)
    } else {
        ICON_FILL
    };
    let icon = tint_icon(ctx, primary);
    let mut ops = Vec::with_capacity(5);

    let Some(data) = maybe_data else {
        ops.push(Op::BarVisible { visible: true });
        ops.push(Op::BarRender {
            tree: container(
                ctx,
                vec![image(&icon), ui::label(json!({ "text": "..." }))],
            ),
        });
        ops.push(Op::BarTooltip {
            value: json!(ctx.tr("tooltip.waiting", &[])),
        });
        ops.push(Op::SetInterval {
            ms: interval_ms(ctx, start),
        });
        return ops;
    };

    if data.remaining == 0 && ctx.config_bool("hide_on_complete", false) {
        ops.push(Op::BarVisible { visible: false });
        ops.push(Op::BarTooltip { value: Value::Null });
        ops.push(Op::SetInterval {
            ms: interval_ms(ctx, start),
        });
        return ops;
    }

    ops.push(Op::BarVisible { visible: true });
    if data.remaining == 0 {
        ops.push(Op::BarRender {
            tree: container(
                ctx,
                vec![ui::node(
                    "glyph",
                    json!({ "name": "check", "size": 14, "color": SUCCESS }),
                    Vec::new(),
                )],
            ),
        });
    } else {
        let mut children = Vec::with_capacity(2);
        if !ctx.config_bool("hide_icon", false) {
            children.push(image(&icon));
        }
        if !ctx.config_bool("hide_text", false) {
            children.push(ui::label(json!({
                "text": data.remaining.to_string(),
                "color": number,
            })));
        }
        ops.push(Op::BarRender {
            tree: container(ctx, children),
        });
    }

    ops.push(Op::BarTooltip {
        value: tooltip(ctx, data, ctx.config_bool("include_new", false)),
    });
    ops.push(Op::SetInterval {
        ms: interval_ms(ctx, start),
    });
    ops
}

fn container(ctx: &Context, children: Vec<UiNode>) -> UiNode {
    let vertical = ctx
        .host("bar.vertical")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let props = json!({ "gap": 6, "align": "center", "justify": "center" });
    if vertical {
        ui::column(props, children)
    } else {
        ui::row(props, children)
    }
}

fn image(path: &str) -> UiNode {
    ui::node(
        "image",
        json!({ "path": path, "width": 16, "height": 16, "fit": "contain" }),
        Vec::new(),
    )
}

fn tooltip(ctx: &Context, data: &anki::Data, include_new: bool) -> Value {
    let mut rows = vec![
        json!({ "key": ctx.tr("tooltip.reviews", &[]), "value": data.reviews.to_string() }),
        json!({ "key": ctx.tr("tooltip.learning", &[]), "value": data.learning.to_string() }),
    ];
    if include_new {
        rows.push(json!({ "key": ctx.tr("tooltip.new", &[]), "value": data.new.to_string() }));
    }
    rows.push(
        json!({ "key": ctx.tr("tooltip.completed", &[]), "value": data.completed.to_string() }),
    );
    rows.push(json!({ "key": ctx.tr("tooltip.total", &[]), "value": data.total.to_string() }));
    json!(rows)
}

fn refresh_millis(ctx: &Context) -> u64 {
    let seconds = ctx
        .config_str("refresh_seconds")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(600)
        .max(5);
    u64::try_from(seconds).unwrap_or(600).saturating_mul(1000)
}

fn interval_ms(ctx: &Context, start: Instant) -> u64 {
    let elapsed =
        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    refresh_millis(ctx).saturating_sub(elapsed).max(1000)
}

fn tint_icon(ctx: &Context, color: &str) -> String {
    if color == ICON_FILL {
        return ICON_SOURCE.to_owned();
    }
    let mut cache = ICON.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((last, path)) = cache.as_ref()
        && last == color
    {
        return path.clone();
    }

    let Some(dir) = ctx.host("pluginDir").and_then(Value::as_str) else {
        return ICON_SOURCE.to_owned();
    };
    let Ok(source) = fs::read_to_string(Path::new(dir).join(ICON_SOURCE))
    else {
        return ICON_SOURCE.to_owned();
    };
    let name = format!("anki-{}.svg", color.trim_start_matches('#'));
    let path = ctx.data_dir.join(name);
    if fs::write(&path, source.replace(ICON_FILL, color)).is_err() {
        return ICON_SOURCE.to_owned();
    }
    let text = path.to_string_lossy().into_owned();
    *cache = Some((color.to_owned(), text.clone()));
    text
}

fn command_sample(args: &[String]) -> ExitCode {
    let (Some(collection), Some(include_arg)) = (args.get(1), args.get(2))
    else {
        eprintln!("usage: banki-helper sample <collection> <include_new> [at]");
        return ExitCode::FAILURE;
    };
    let include_new = include_arg == "1";
    let now = if let Some(value) = args.get(3) {
        let Ok(now) = value.parse::<i64>() else {
            eprintln!("invalid timestamp: {value}");
            return ExitCode::FAILURE;
        };
        now
    } else {
        anki::now_seconds()
    };
    let path = match anki::collection_path(collection, Path::new(".")) {
        Ok(path) => path,
        Err(error) => {
            println!("{}", json!({ "ok": false, "error": error.to_string() }));
            return ExitCode::FAILURE;
        }
    };
    match anki::sample(&path, include_new, now) {
        Ok(data) => {
            println!("{}", data_json(&data));
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                json!({ "ok": false, "error": error.to_string(), "at": now })
            );
            ExitCode::FAILURE
        }
    }
}

fn data_json(data: &anki::Data) -> Value {
    json!({
        "reviews": data.reviews,
        "learning": data.learning,
        "new": data.new,
        "remaining": data.remaining,
        "completed": data.completed,
        "total": data.total,
        "at": data.at,
        "ok": true,
    })
}

fn command_bench(args: &[String]) -> ExitCode {
    let (Some(collection), Some(iterations_arg)) = (args.get(1), args.get(2))
    else {
        eprintln!(
            "usage: banki-helper bench <collection> <iterations> [include_new]"
        );
        return ExitCode::FAILURE;
    };
    let iterations = iterations_arg.parse::<usize>().unwrap_or(1000);
    if iterations == 0 {
        eprintln!("iterations must be at least 1");
        return ExitCode::FAILURE;
    }
    let include_new = args.get(3).is_some_and(|value| value == "1");
    let path = match anki::collection_path(collection, Path::new(".")) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let now = anki::now_seconds();
    let mut samples = Vec::with_capacity(iterations.saturating_sub(1));
    let mut first = 0.0;
    for index in 0..iterations {
        let start = Instant::now();
        if let Err(error) = anki::sample(&path, include_new, now) {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
        let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0;
        if index == 0 {
            first = elapsed;
        } else {
            samples.push(elapsed);
        }
    }
    samples.sort_by(f64::total_cmp);
    if samples.is_empty() {
        println!("n=0 first={first:.1}us median=n/a p95=n/a min=n/a max=n/a");
        return ExitCode::SUCCESS;
    }
    let at = |percent: usize| {
        let rank = samples
            .len()
            .saturating_mul(percent)
            .saturating_add(99)
            .saturating_div(100);
        let index = rank.saturating_sub(1).min(samples.len().saturating_sub(1));
        samples.get(index).copied().unwrap_or(0.0)
    };
    println!(
        "n={} first={:.1}us median={:.1}us p95={:.1}us min={:.1}us max={:.1}us",
        samples.len(),
        first,
        at(50),
        at(95),
        samples.first().copied().unwrap_or(0.0),
        samples.last().copied().unwrap_or(0.0),
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use noctalia_rs::Parts;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn context(config: Value, host: Value) -> Context {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir()
            .join(format!("banki-test-{}-{unique}", process::id()));
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
            json!({
                "tooltip": {
                    "reviews": "Reviews due now",
                    "learning": "Learning due now",
                    "new": "New (no deck limits)",
                    "completed": "Completed today",
                    "total": "Total today",
                    "waiting": "Waiting for Anki data"
                }
            }),
        )
    }

    fn data(remaining: i64) -> anki::Data {
        anki::Data {
            reviews: 1,
            learning: 2,
            new: 3,
            remaining,
            completed: 4,
            total: remaining.saturating_add(4),
            at: 0,
        }
    }

    #[test]
    fn test_waiting_render() {
        let ctx = context(json!({}), json!({ "bar": { "vertical": false } }));

        let ops = render_ops(&ctx, None, Instant::now());

        assert!(matches!(ops[0], Op::BarVisible { visible: true }));
        assert!(matches!(ops[1], Op::BarRender { .. }));
        assert!(matches!(ops[2], Op::BarTooltip { .. }));
        assert!(matches!(ops[3], Op::SetInterval { .. }));
    }

    #[test]
    fn test_prepare_key_keeps_existing_value() {
        let ctx = context(
            json!({ "sync_enabled": true, "sync_key": "secret" }),
            json!({}),
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
    fn test_hidden_complete_render_clears_tooltip() {
        let ctx = context(
            json!({ "hide_on_complete": true }),
            json!({ "bar": { "vertical": false } }),
        );

        let ops = render_ops(&ctx, Some(&data(0)), Instant::now());

        assert!(matches!(ops[0], Op::BarVisible { visible: false }));
        assert!(matches!(ops[1], Op::BarTooltip { value: Value::Null }));
    }

    #[test]
    fn test_complete_render_uses_check_glyph() {
        let ctx = context(
            json!({ "follow_accent": false }),
            json!({ "bar": { "vertical": true } }),
        );

        let ops = render_ops(&ctx, Some(&data(0)), Instant::now());

        let Op::BarRender { tree } = &ops[1] else {
            panic!("render op");
        };
        let value = serde_json::to_value(tree).expect("tree");
        assert_eq!(value["kind"], "column");
        assert_eq!(value["children"][0]["kind"], "glyph");
        assert_eq!(value["children"][0]["props"]["name"], "check");
    }

    #[test]
    fn test_incomplete_render_hides_parts() {
        let ctx = context(
            json!({ "hide_icon": true, "follow_accent": false }),
            json!({ "bar": { "vertical": false } }),
        );

        let ops = render_ops(&ctx, Some(&data(5)), Instant::now());

        let Op::BarRender { tree } = &ops[1] else {
            panic!("render op");
        };
        let value = serde_json::to_value(tree).expect("tree");
        assert_eq!(value["children"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["children"][0]["kind"], "label");
    }
}
