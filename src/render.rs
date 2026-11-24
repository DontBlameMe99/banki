use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use noctalia_rs::ui::UiNode;
use noctalia_rs::{Context, Op, ui};
use serde_json::{Value, json};

use crate::anki;
use crate::lock;

const ICON_SOURCE: &str = "assets/anki.svg";
const ICON_FILL: &str = "#80C2EE";
const SUCCESS: &str = "#4CAF50";
const ACCENT_ROLE: &str = "primary";
const DEFAULT_REFRESH_SECONDS: i64 = 600;
const MIN_REFRESH_SECONDS: i64 = 5;
const MIN_INTERVAL_MS: u64 = 1000;

static LAST_ICON: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Bar ops for one refresh. `data` is `None` until the first successful read.
pub fn ops(
    ctx: &Context,
    data: Option<&anki::Data>,
    started: Instant,
) -> Vec<Op> {
    let colors = Colors::read(ctx);
    let mut ops = match data {
        None => waiting(ctx, &colors),
        Some(current) if current.remaining > 0 => counts(ctx, current, &colors),
        Some(_) if ctx.config_bool("hide_on_complete", false) => hidden(),
        Some(current) => complete(ctx, current),
    };
    ops.push(Op::SetInterval {
        ms: delay_ms(ctx, started),
    });
    ops
}

/// Label color as a role name and icon path, both from the palette when
/// `follow_accent` is on.
struct Colors {
    label: &'static str,
    icon: String,
}

impl Colors {
    fn read(ctx: &Context) -> Self {
        if !ctx.config_bool("follow_accent", true) {
            return Self {
                label: ICON_FILL,
                icon: ICON_SOURCE.to_owned(),
            };
        }
        let accent = ctx
            .color(ACCENT_ROLE)
            .and_then(Value::as_str)
            .unwrap_or(ICON_FILL);
        Self {
            label: ACCENT_ROLE,
            icon: icon_path(ctx, accent),
        }
    }
}

fn waiting(ctx: &Context, colors: &Colors) -> Vec<Op> {
    vec![
        Op::BarVisible { visible: true },
        Op::BarRender {
            tree: container(
                ctx,
                vec![image(&colors.icon), ui::label(json!({ "text": "..." }))],
            ),
        },
        Op::BarTooltip {
            value: json!(ctx.tr("tooltip.waiting", &[])),
        },
    ]
}

fn hidden() -> Vec<Op> {
    vec![
        Op::BarVisible { visible: false },
        Op::BarTooltip { value: Value::Null },
    ]
}

fn counts(ctx: &Context, data: &anki::Data, colors: &Colors) -> Vec<Op> {
    let mut children = Vec::with_capacity(2);
    if !ctx.config_bool("hide_icon", false) {
        children.push(image(&colors.icon));
    }
    if !ctx.config_bool("hide_text", false) {
        children.push(ui::label(json!({
            "text": data.remaining.to_string(),
            "color": colors.label,
        })));
    }

    vec![
        Op::BarVisible { visible: true },
        Op::BarRender {
            tree: container(ctx, children),
        },
        Op::BarTooltip {
            value: tooltip(ctx, data),
        },
    ]
}

fn complete(ctx: &Context, data: &anki::Data) -> Vec<Op> {
    vec![
        Op::BarVisible { visible: true },
        Op::BarRender {
            tree: container(
                ctx,
                vec![ui::node(
                    "glyph",
                    json!({ "name": "check", "size": 14, "color": SUCCESS }),
                    Vec::new(),
                )],
            ),
        },
        Op::BarTooltip {
            value: tooltip(ctx, data),
        },
    ]
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

fn tooltip(ctx: &Context, data: &anki::Data) -> Value {
    let mut rows = vec![
        ("tooltip.reviews", data.reviews),
        ("tooltip.learning", data.learning),
    ];
    if ctx.config_bool("include_new", false) {
        rows.push(("tooltip.new", data.new));
    }
    rows.push(("tooltip.completed", data.completed));
    rows.push(("tooltip.total", data.total));
    Value::Array(
        rows.into_iter()
            .map(|(key, value)| {
                json!({ "key": ctx.tr(key, &[]), "value": value.to_string() })
            })
            .collect(),
    )
}

/// Milliseconds until the next refresh, never below `MIN_INTERVAL_MS`.
fn delay_ms(ctx: &Context, started: Instant) -> u64 {
    let elapsed =
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    refresh_period_ms(ctx)
        .saturating_sub(elapsed)
        .max(MIN_INTERVAL_MS)
}

fn refresh_period_ms(ctx: &Context) -> u64 {
    let seconds = ctx
        .config_str("refresh_seconds")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_REFRESH_SECONDS)
        .max(MIN_REFRESH_SECONDS);
    u64::try_from(seconds)
        .unwrap_or_default()
        .saturating_mul(1000)
}

/// Path of an icon tinted with `accent`, written once per color.
fn icon_path(ctx: &Context, accent: &str) -> String {
    if accent == ICON_FILL {
        return ICON_SOURCE.to_owned();
    }
    let mut cache = lock(&LAST_ICON);
    if let Some((last, path)) = cache.as_ref()
        && last == accent
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
    let name = format!("anki-{}.svg", accent.trim_start_matches('#'));
    let path = ctx.data_dir.join(name);
    if fs::write(&path, source.replace(ICON_FILL, accent)).is_err() {
        return ICON_SOURCE.to_owned();
    }
    let text = path.to_string_lossy().into_owned();
    *cache = Some((accent.to_owned(), text.clone()));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::context;

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

        let ops = ops(&ctx, None, Instant::now());

        assert!(matches!(ops[0], Op::BarVisible { visible: true }));
        assert!(matches!(ops[1], Op::BarRender { .. }));
        assert!(matches!(ops[2], Op::BarTooltip { .. }));
        assert!(matches!(ops[3], Op::SetInterval { .. }));
    }

    #[test]
    fn test_hidden_complete_render_clears_tooltip() {
        let ctx = context(
            json!({ "hide_on_complete": true }),
            json!({ "bar": { "vertical": false } }),
        );

        let ops = ops(&ctx, Some(&data(0)), Instant::now());

        assert!(matches!(ops[0], Op::BarVisible { visible: false }));
        assert!(matches!(ops[1], Op::BarTooltip { value: Value::Null }));
    }

    #[test]
    fn test_complete_render_uses_check_glyph() {
        let ctx = context(
            json!({ "follow_accent": false }),
            json!({ "bar": { "vertical": true } }),
        );

        let ops = ops(&ctx, Some(&data(0)), Instant::now());

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

        let ops = ops(&ctx, Some(&data(5)), Instant::now());

        let Op::BarRender { tree } = &ops[1] else {
            panic!("render op");
        };
        let value = serde_json::to_value(tree).expect("tree");
        assert_eq!(value["children"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["children"][0]["kind"], "label");
    }

    #[test]
    fn test_tooltip_lists_new_only_when_included() {
        let included = context(json!({ "include_new": true }), Value::Null);
        let excluded = context(json!({}), Value::Null);

        let rows = tooltip(&included, &data(5));
        let fewer = tooltip(&excluded, &data(5));

        assert_eq!(rows.as_array().map(Vec::len), Some(5));
        assert_eq!(fewer.as_array().map(Vec::len), Some(4));
        assert_eq!(rows[0]["key"], "Reviews due now");
        assert_eq!(rows[2]["key"], "New (no deck limits)");
        assert_eq!(rows[4]["value"], "9");
    }

    #[test]
    fn test_refresh_period_clamps_short_values() {
        let short = context(json!({ "refresh_seconds": "1" }), Value::Null);
        let invalid = context(json!({ "refresh_seconds": "-10" }), Value::Null);
        let unset = context(json!({}), Value::Null);

        assert_eq!(refresh_period_ms(&short), 5_000);
        assert_eq!(refresh_period_ms(&invalid), 5_000);
        assert_eq!(refresh_period_ms(&unset), 600_000);
    }
}
