#!/usr/bin/env python3
"""Emit the noctalia-rs plugin and apply compatibility fixes to its Luau."""

import subprocess
import sys
from pathlib import Path

HELPER = sys.argv[1] if len(sys.argv) > 1 else "target/release/banki-helper"
ROOT = Path(__file__).resolve().parents[2]

subprocess.run(
    [str(ROOT / HELPER), "emit", "banki", "banki-helper"],
    check=True,
    cwd=ROOT,
)

bar = ROOT / "banki/bar.luau"
text = bar.read_text()
if "--!nolint FunctionUnused" not in text:
    lines = text.splitlines()
    lines.insert(1, "--!nolint FunctionUnused")
    bar.write_text("\n".join(lines) + "\n")

bridge = ROOT / "banki/lib/bridge.luau"
text = bridge.read_text()
for line in (
    '  "surface_variant",\n',
    '  "outline",\n',
    '  "shell.lang",\n',
    '  "shell.language",\n',
    '  "general.language",\n',
    '  "desktop_widgets.widget",\n',
):
    text = text.replace(line, "")
bridge.write_text(text)
