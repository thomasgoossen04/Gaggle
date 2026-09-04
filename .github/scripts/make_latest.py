#!/usr/bin/env python3
"""Compose the release descriptor the launcher fetches.

Usage: make_latest.py <version> <tag> <channel> <dist-dir>

  <version>   e.g. 2.0.deadbee  or  2.0.deadbee-beta
  <tag>       the release tag the assets live under (v2.0.deadbee, or "beta")
  <channel>   "stable" | "beta"  (informational, written into the descriptor)
  <dist-dir>  holds, per platform:
                gaggle-<platform>.zip
                gaggle-<platform>.zip.sha256   (lowercase hex digest, first token)

where <platform> is one of the launcher's keys:
    linux-x86_64  windows-x86_64  macos-aarch64  macos-x86_64

Prints the JSON to stdout.
"""

import datetime as _dt
import json
import pathlib
import sys

REPO = "thomasgoossen04/Gaggle"
PLATFORMS = ("linux-x86_64", "windows-x86_64", "macos-aarch64", "macos-x86_64")


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
    for name in PLATFORMS:
        archive = dist / f"gaggle-{name}.zip"
        digest = dist / f"gaggle-{name}.zip.sha256"
        if not archive.is_file() or not digest.is_file():
            print(f"warning: missing artifact for {name}, skipping", file=sys.stderr)
            continue
        platforms[name] = {
            "url": f"{base}/{archive.name}",
            "sha256": digest.read_text().split()[0].strip().lower(),
            "size": archive.stat().st_size,
        }

    if not platforms:
        print("error: no platform artifacts found", file=sys.stderr)
        return 1

    doc = {
        "version": version,
        "channel": channel,
        "notes": f"Automated {channel} release {version}",
        "pub_date": _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }
    print(json.dumps(doc, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
