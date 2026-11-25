#!/usr/bin/env python3
"""Emit the noctalia-rs plugin and apply compatibility fixes to its Luau."""

import subprocess
import sys
import tomllib
from pathlib import Path

HELPER = sys.argv[1] if len(sys.argv) > 1 else "target/release/banki-helper"
ROOT = Path(__file__).resolve().parents[2]

subprocess.run(
    [str(ROOT / HELPER), "emit", "banki", "banki-helper"],
    check=True,
    cwd=ROOT,
)

for entry in sorted(ROOT.glob("banki/*.luau")):
    text = entry.read_text()
    if "--!nolint FunctionUnused" not in text:
        lines = text.splitlines()
        lines.insert(1, "--!nolint FunctionUnused")
        entry.write_text("\n".join(lines) + "\n")

manifest = tomllib.loads((ROOT / "banki/plugin.toml").read_text())
entry_id = f"{manifest['id']}:{manifest['widget'][0]['id']}"

bar = ROOT / "banki/bar.luau"
text = bar.read_text()
load = 'bridge.call("render", { source = "load" })'
setup = next(
    (line for line in text.splitlines() if line.startswith("bridge.setup(")),
    None,
)
if setup is None:
    raise SystemExit("emit.py: generated entry no longer sets up the bridge")
# The entry owns the timer so a failed first helper response cannot strand it.
interval = (
    "local function refreshInterval()\n"
    '  local seconds = tonumber(noctalia.getConfig("refresh_seconds")) or 600\n'
    "  noctalia.setUpdateInterval(math.max(seconds, 5) * 1000)\n"
    "end\n"
    "refreshInterval()"
)
text = text.replace(setup, setup + "\n" + interval, 1)
waiting = (
    "local waiting = barWidget.isVertical() and ui.column or ui.row\n"
    "barWidget.setVisible(true)\n"
    'barWidget.render(waiting({ gap = 6, align = "center", justify = "center" }, {\n'
    '  ui.image({ path = "assets/anki.svg", width = 16, height = 16, fit = "contain" }),\n'
    '  ui.label({ text = "..." }),\n'
    "}))\n"
    'barWidget.setTooltip(noctalia.tr("tooltip.waiting"))'
)
if load not in text:
    raise SystemExit("emit.py: generated entry no longer loads the render")
text = text.replace(load, waiting + "\n" + load)
config_changed = 'function onConfigChanged() bridge.call("config_changed") end'
if config_changed not in text:
    raise SystemExit("emit.py: generated entry no longer has onConfigChanged")
text = text.replace(
    config_changed,
    "function onConfigChanged()\n"
    "  refreshInterval()\n"
    '  bridge.call("config_changed")\n'
    "end",
)
right = 'function onRightClick() bridge.call("right_click") end'
direct = (
    "function onRightClick()\n"
    f'  noctalia.runAsync({{ "noctalia", "msg", "plugin", "{entry_id}", "all", "refresh" }})\n'
    "end"
)
if right not in text:
    raise SystemExit("emit.py: generated entry no longer has onRightClick")
text = text.replace(right, direct)
bar.write_text(text)

bridge = ROOT / "banki/lib/bridge.luau"
text = bridge.read_text()
for line in (
    '  "surface_variant",\n',
    '  "outline",\n',
    '  "shell.lang",\n',
    '  "shell.language",\n',
    '  "general.language",\n',
    '  "desktop_widgets.widget",\n',
    '  clipboard = invoke(noctalia, "clipboardText"),\n',
):
    if line not in text:
        raise SystemExit(
            f"emit.py: generated bridge no longer contains {line.strip()}"
        )
    text = text.replace(line, "")
bridge.write_text(text)
