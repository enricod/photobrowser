#!/usr/bin/env python3
"""Writes cargo-sources.json for flatpak-builder from Cargo.lock.

Every crates.io package in Cargo.lock becomes a .crate archive (URL and sha256 straight
from the lock file) unpacked under cargo/vendor/, plus the .cargo-checksum.json Cargo
needs for vendored sources, and a Cargo config that points crates.io at that directory.
Builds can then run `cargo --offline` with no network, as Flathub requires.

Standard library only, no network access. Rerun after every Cargo.lock change:

    python3 build-aux/flatpak/gen-cargo-sources.py Cargo.lock build-aux/flatpak/cargo-sources.json
"""

import json
import sys
import tomllib

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
CONFIG = """[source.vendored-sources]
directory = "cargo/vendor"

[source.crates-io]
replace-with = "vendored-sources"
"""


def main(lock_path: str, out_path: str) -> None:
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    sources = []
    for pkg in lock.get("package", []):
        source = pkg.get("source")
        if source is None:
            continue  # the workspace's own crate
        if source != CRATES_IO:
            sys.exit(f"unsupported source for {pkg['name']}: {source}")
        name, version, checksum = pkg["name"], pkg["version"], pkg["checksum"]
        dest = f"cargo/vendor/{name}-{version}"
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    sources.append(
        {"type": "inline", "contents": CONFIG, "dest": "cargo", "dest-filename": "config.toml"}
    )
    with open(out_path, "w") as f:
        json.dump(sources, f, indent=4)
        f.write("\n")
    print(f"{len(sources) // 2} crates -> {out_path}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
