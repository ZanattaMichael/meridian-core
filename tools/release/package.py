#!/usr/bin/env python3
"""Packs one platform's release archive.

Given the directory a `cargo build --profile dist --target <triple>` left its
binaries in, this signs every plugin and writes

    <out>/meridian-<version>-<triple>.tar.gz      (.zip for Windows)

holding one directory, `meridian-<version>-<triple>/`, with the `meridian`
command, every plugin beside it, each plugin's `.sig`, the public key they
were signed with, the licence and a short README.

The plugins that ship are the ones under `plugins/`; test helpers that happen
to be built with the workspace are left out.

    python3 tools/release/package.py \\
        --version 0.1.0 --target x86_64-unknown-linux-musl \\
        --bin-dir target/x86_64-unknown-linux-musl/dist \\
        --signer target/release/meridian --key release.key --out dist

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import pathlib
import shutil
import subprocess
import sys
import tarfile
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]

README = """Meridian {version} for {target}

This directory is a complete installation. Keep the files together: the
meridian command finds its plugins in the directory it is in.

  meridian --version
  meridian plugins

Each plugin has a signature beside it (the .sig file), made when this release
was built. This build of meridian carries the matching public key, which is
also in release-key.pub, and by default it starts only plugins whose signature
passes. To use a plugin from somewhere else, trust its signer's key:

  meridian plugins --trusted-key their-key.pub

Documentation: https://github.com/ZanattaMichael/meridian-core/tree/main/docs
"""


def shipped_plugins() -> list[str]:
    """The binary names of the plugins under plugins/, sorted."""
    names = []
    for manifest in sorted((ROOT / "plugins").glob("*/Cargo.toml")):
        for line in manifest.read_text(encoding="utf-8").splitlines():
            if line.startswith("name = "):
                names.append(line.split('"')[1])
                break
    if not names:
        sys.exit("no plugins found under plugins/")
    return names


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", required=True, help="the Rust target triple")
    parser.add_argument("--bin-dir", required=True, type=pathlib.Path)
    parser.add_argument("--out", required=True, type=pathlib.Path)
    parser.add_argument("--signer", type=pathlib.Path, help="a meridian binary that runs on this machine")
    parser.add_argument("--key", type=pathlib.Path, help="the secret key to sign the plugins with")
    args = parser.parse_args()
    if bool(args.signer) != bool(args.key):
        sys.exit("--signer and --key go together")

    windows = "windows" in args.target
    suffix = ".exe" if windows else ""
    name = f"meridian-{args.version}-{args.target}"
    stage = args.out / "stage" / name
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)

    plugins = shipped_plugins()
    for binary in ["meridian", *plugins]:
        source = args.bin_dir / f"{binary}{suffix}"
        if not source.is_file():
            sys.exit(f"{source} was not built")
        shutil.copy2(source, stage / source.name)
        (stage / source.name).chmod(0o755)

    if args.signer:
        staged = [str(stage / f"{plugin}{suffix}") for plugin in plugins]
        subprocess.run(
            [str(args.signer), "signing", "sign", "--key", str(args.key), *staged],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        public = args.key.with_suffix(".pub")
        if not public.is_file():
            sys.exit(f"{public}, the public half of the key, is not beside it")
        shutil.copy2(public, stage / "release-key.pub")

    shutil.copy2(ROOT / "LICENSE", stage / "LICENSE")
    (stage / "README.txt").write_text(
        README.format(version=args.version, target=args.target), encoding="utf-8"
    )

    files = sorted(stage.iterdir())
    if windows:
        archive = args.out / f"{name}.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as out:
            for file in files:
                out.write(file, f"{name}/{file.name}")
    else:
        archive = args.out / f"{name}.tar.gz"

        def owned_by_root(info: tarfile.TarInfo) -> tarfile.TarInfo:
            # Whoever built the archive is not who should own what it unpacks.
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            return info

        with tarfile.open(archive, "w:gz") as out:
            for file in files:
                out.add(file, f"{name}/{file.name}", filter=owned_by_root)
    shutil.rmtree(args.out / "stage")
    print(archive)


if __name__ == "__main__":
    main()
