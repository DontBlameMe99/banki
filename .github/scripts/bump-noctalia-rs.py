#!/usr/bin/env python3
"""Update the noctalia-rs dependency to a released version."""

import re
import subprocess
import sys
from pathlib import Path

PACKAGES = ("noctalia-rs", "noctalia-rs-build")


def main():
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {Path(sys.argv[0]).name} <version>")

    version = sys.argv[1].strip().removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SystemExit(f"invalid release version: {version}")

    major_minor = ".".join(version.split(".")[:2])
    manifest = Path("Cargo.toml")
    text = manifest.read_text()

    for package in PACKAGES:
        pattern = re.compile(
            rf'^({re.escape(package)}\s*=\s*)"[^"]+"$',
            re.MULTILINE,
        )
        if not pattern.search(text):
            raise SystemExit(f"missing dependency: {package}")
        text = pattern.sub(rf'\1"{major_minor}"', text, count=1)

    manifest.write_text(text)

    for package in PACKAGES:
        subprocess.run(
            ["cargo", "update", "-p", package, "--precise", version],
            check=True,
        )

    print(f"noctalia-rs {version} pinned in Cargo.lock ({major_minor} range)")


if __name__ == "__main__":
    main()
