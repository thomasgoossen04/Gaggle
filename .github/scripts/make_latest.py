#!/usr/bin/env python3
"""Compose the release descriptor both launchers fetch.

Usage: make_latest.py <version> <tag> <channel> <dist-dir>

  <version>   e.g. 2.0.deadbee  or  2.0.deadbee-beta
  <tag>       the release tag the assets live under (v2.0.deadbee, or "beta")
  <channel>   "stable" | "beta"  (informational, written into the descriptor)
  <dist-dir>  holds, per platform:
                gaggle-<platform>.zip
                gaggle-<platform>.zip.sha256          (lowercase hex digest, first token)
                gaggle-accelerator-<platform>[.exe]        (standalone daemon binary)
                gaggle-accelerator-<platform>[.exe].sha256

where <platform> is one of the launchers' keys:
    linux-x86_64  windows-x86_64  macos-aarch64  macos-x86_64

One descriptor serves two independent consumers: `gaggle-launcher` reads
`platforms` (the GUI+launcher zip); `gaggle-accelerator-launcher` reads
`accelerator` (the standalone daemon binary) and ignores `platforms` entirely.
A platform missing from the dist dir is simply omitted from its map, not fatal.

Prints the JSON to stdout.
"""

import datetime as _dt
import json
import pathlib
import sys

REPO = "thomasgoossen04/Gaggle"
PLATFORMS = ("linux-x86_64", "windows-x86_64", "macos-aarch64", "macos-x86_64")


def exe_suffix(platform: str) -> str:
    return ".exe" if platform.startswith("windows") else ""


def asset_map(dist: pathlib.Path, base: str, filename: str) -> dict | None:
    archive = dist / filename
    digest = dist / f"{filename}.sha256"
    if not archive.is_file() or not digest.is_file():
        return None
    return {
        "url": f"{base}/{archive.name}",
        "sha256": digest.read_text().split()[0].strip().lower(),
        "size": archive.stat().st_size,
    }


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__, file=sys.stderr)
        return 2

    version, tag, channel, dist = (
        sys.argv[1],
        sys.argv[2],
        sys.argv[3],
        pathlib.Path(sys.argv[4]),
    )
    base = f"https://github.com/{REPO}/releases/download/{tag}"

    platforms = {}
    accelerator = {}
    for name in PLATFORMS:
        gui = asset_map(dist, base, f"gaggle-{name}.zip")
        if gui is None:
            print(f"warning: missing GUI artifact for {name}, skipping", file=sys.stderr)
        else:
            platforms[name] = gui

        accel = asset_map(dist, base, f"gaggle-accelerator-{name}{exe_suffix(name)}")
        if accel is None:
            print(f"warning: missing accelerator artifact for {name}, skipping", file=sys.stderr)
        else:
            accelerator[name] = accel

    if not platforms and not accelerator:
        print("error: no platform artifacts found", file=sys.stderr)
        return 1

    doc = {
        "version": version,
        "channel": channel,
        "notes": f"Automated {channel} release {version}",
        "pub_date": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
        "accelerator": accelerator,
    }
    print(json.dumps(doc, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
