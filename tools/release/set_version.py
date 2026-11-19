#!/usr/bin/env python3
"""Sets the workspace version for one build, as the nightly workflow does.

Rewrites `version` under `[workspace.package]` in Cargo.toml and the version
of every workspace crate in Cargo.lock (the entries with no `source`), so
`cargo build --locked` still passes and `meridian --version` prints it.

    python3 tools/release/set_version.py 0.1.1-nightly.20261119.gabc1234

Only the Python standard library is used.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
# Semver with a pre-release and no build metadata: a `+` does not survive
# every download URL the archives are fetched from.
VERSION = re.compile(r"^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$")


def main(version: str) -> None:
    if not VERSION.match(version):
        sys.exit(f"set_version.py: {version!r} is not a version (semver, no '+')")

    toml = ROOT / "Cargo.toml"
    text = toml.read_text(encoding="utf-8")
    text, count = re.subn(
        r'(\[workspace\.package\]\n(?:[^\[\n].*\n|\n)*?version = )"[^"]*"',
        rf'\g<1>"{version}"',
        text,
        count=1,
    )
    if count != 1:
        sys.exit("set_version.py: no version under [workspace.package] in Cargo.toml")
    toml.write_text(text, encoding="utf-8", newline="\n")

    lock = ROOT / "Cargo.lock"
    packages = lock.read_text(encoding="utf-8").split("\n[[package]]\n")
    changed = 0
    for i, package in enumerate(packages):
        if i == 0 or "\nsource = " in package:
            continue
        packages[i] = re.sub(r'^(name = "[^"]+"\nversion = )"[^"]*"', rf'\g<1>"{version}"', package)
        changed += 1
    if changed == 0:
        sys.exit("set_version.py: no workspace crate in Cargo.lock")
    lock.write_text("\n[[package]]\n".join(packages), encoding="utf-8", newline="\n")
    print(f"workspace version {version}: Cargo.toml and {changed} crates in Cargo.lock")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: set_version.py <version>")
    main(sys.argv[1])
