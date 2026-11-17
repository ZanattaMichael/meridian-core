#!/usr/bin/env python3
"""Writes what goes beside a release's archives: the checksums, and the
manifests package managers install from.

Given a directory holding the six `meridian-<version>-<triple>` archives, this
writes into it

    meridian-<version>-checksums.txt    SHA-256 of every archive
    VERSION                             the version, for the install scripts

and into `<dir>/packaging/`

    homebrew/meridian.rb                a Homebrew formula
    scoop/meridian.json                 a Scoop manifest
    winget/<id>.yaml, .installer.yaml, .locale.en-US.yaml

Each manifest names the archives by the URL they will have once the release
is published, and by their checksum. Nothing here publishes anything: the
files are release assets, and whether they are sent to a tap, a bucket or
winget-pkgs is the maintainer's decision (docs/contributing/releasing.md).

    python3 tools/release/manifests.py --version 0.1.0 --dir dist

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import sys

REPOSITORY = "ZanattaMichael/meridian-core"
HOMEPAGE = f"https://github.com/{REPOSITORY}"
DESCRIPTION = "One document, any tool: compile Meridian documents and run the tools they target"
LICENSE = "MIT"
WINGET_ID = "ZanattaMichael.Meridian"
WINGET_SCHEMA = "1.6.0"

# The platforms a release is built for: the Rust target, and what each
# package manager calls it.
TARGETS = {
    "x86_64-unknown-linux-musl": "tar.gz",
    "aarch64-unknown-linux-musl": "tar.gz",
    "x86_64-apple-darwin": "tar.gz",
    "aarch64-apple-darwin": "tar.gz",
    "x86_64-pc-windows-msvc": "zip",
    "aarch64-pc-windows-msvc": "zip",
}


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def homebrew(version: str, url, sums) -> str:
    def block(target: str, indent: str) -> str:
        return f'{indent}url "{url(target)}"\n{indent}sha256 "{sums[target]}"'

    return f"""# Written by tools/release/manifests.py for Meridian {version}. Do not edit:
# the next release replaces it.
class Meridian < Formula
  desc "{DESCRIPTION}"
  homepage "{HOMEPAGE}"
  version "{version}"
  license "{LICENSE}"

  on_macos do
    on_arm do
{block("aarch64-apple-darwin", "      ")}
    end
    on_intel do
{block("x86_64-apple-darwin", "      ")}
    end
  end

  on_linux do
    on_arm do
{block("aarch64-unknown-linux-musl", "      ")}
    end
    on_intel do
{block("x86_64-unknown-linux-musl", "      ")}
    end
  end

  def install
    # The command and its plugins stay together: meridian looks for plugins
    # in the directory it is really in, following the link in bin.
    libexec.install Dir["*"]
    bin.install_symlink libexec/"meridian"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/meridian --version")
    assert_match "signature: verified", shell_output("#{{bin}}/meridian plugins")
  end
end
"""


def scoop(version: str, url, sums) -> str:
    def arch(target: str) -> dict:
        return {
            "url": url(target),
            "hash": sums[target],
            "extract_dir": f"meridian-{version}-{target}",
        }

    def auto(target: str) -> dict:
        return {
            "url": f"{HOMEPAGE}/releases/download/v$version/meridian-$version-{target}.zip",
            "extract_dir": f"meridian-$version-{target}",
        }

    manifest = {
        "version": version,
        "description": DESCRIPTION,
        "homepage": HOMEPAGE,
        "license": LICENSE,
        "architecture": {
            "64bit": arch("x86_64-pc-windows-msvc"),
            "arm64": arch("aarch64-pc-windows-msvc"),
        },
        "bin": "meridian.exe",
        "checkver": {"github": HOMEPAGE},
        "autoupdate": {
            "architecture": {
                "64bit": auto("x86_64-pc-windows-msvc"),
                "arm64": auto("aarch64-pc-windows-msvc"),
            },
            "hash": {"url": "$baseurl/meridian-$version-checksums.txt"},
        },
    }
    return json.dumps(manifest, indent=4) + "\n"


def winget(version: str, url, sums) -> dict[str, str]:
    header = (
        "# Written by tools/release/manifests.py. Do not edit: the next release replaces it.\n"
        "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{kind}.{schema}.schema.json\n"
    )

    def installer(architecture: str, target: str) -> str:
        return (
            f"  - Architecture: {architecture}\n"
            f"    InstallerUrl: {url(target)}\n"
            f"    InstallerSha256: {sums[target].upper()}\n"
            f"    NestedInstallerFiles:\n"
            f"      - RelativeFilePath: meridian-{version}-{target}\\meridian.exe\n"
            f"        PortableCommandAlias: meridian\n"
        )

    tail = f"ManifestVersion: {WINGET_SCHEMA}\n"
    return {
        f"{WINGET_ID}.yaml": header.format(kind="version", schema=WINGET_SCHEMA)
        + f"PackageIdentifier: {WINGET_ID}\n"
        f"PackageVersion: {version}\n"
        "DefaultLocale: en-US\n"
        "ManifestType: version\n" + tail,
        f"{WINGET_ID}.installer.yaml": header.format(kind="installer", schema=WINGET_SCHEMA)
        + f"PackageIdentifier: {WINGET_ID}\n"
        f"PackageVersion: {version}\n"
        "InstallerType: zip\n"
        "NestedInstallerType: portable\n"
        "Installers:\n"
        + installer("x64", "x86_64-pc-windows-msvc")
        + installer("arm64", "aarch64-pc-windows-msvc")
        + "ManifestType: installer\n"
        + tail,
        f"{WINGET_ID}.locale.en-US.yaml": header.format(
            kind="defaultLocale", schema=WINGET_SCHEMA
        )
        + f"PackageIdentifier: {WINGET_ID}\n"
        f"PackageVersion: {version}\n"
        "PackageLocale: en-US\n"
        "Publisher: Michael Zanatta\n"
        f"PublisherUrl: https://github.com/{REPOSITORY.split('/')[0]}\n"
        f"PublisherSupportUrl: {HOMEPAGE}/issues\n"
        "PackageName: Meridian\n"
        f"PackageUrl: {HOMEPAGE}\n"
        f"License: {LICENSE}\n"
        f"LicenseUrl: {HOMEPAGE}/blob/main/LICENSE\n"
        # Quoted, because the description holds a colon.
        f"ShortDescription: {json.dumps(DESCRIPTION)}\n"
        "Moniker: meridian\n"
        "Tags:\n  - infrastructure-as-code\n  - configuration-management\n  - devops\n"
        f"ReleaseNotesUrl: {HOMEPAGE}/releases/tag/v{version}\n"
        "ManifestType: defaultLocale\n" + tail,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--version", required=True)
    parser.add_argument("--dir", required=True, type=pathlib.Path)
    args = parser.parse_args()
    version = args.version

    def file_name(target: str) -> str:
        return f"meridian-{version}-{target}.{TARGETS[target]}"

    def url(target: str) -> str:
        return f"{HOMEPAGE}/releases/download/v{version}/{file_name(target)}"

    sums = {}
    for target in TARGETS:
        archive = args.dir / file_name(target)
        if not archive.is_file():
            sys.exit(f"{archive} is missing: a release has an archive for every platform")
        sums[target] = sha256(archive)

    # The format `sha256sum --check` and `shasum -a 256 --check` both read.
    (args.dir / f"meridian-{version}-checksums.txt").write_text(
        "".join(f"{sums[target]}  {file_name(target)}\n" for target in TARGETS),
        encoding="utf-8",
        newline="\n",
    )
    (args.dir / "VERSION").write_text(f"{version}\n", encoding="utf-8", newline="\n")

    written = {
        "homebrew/meridian.rb": homebrew(version, url, sums),
        "scoop/meridian.json": scoop(version, url, sums),
    }
    for name, text in winget(version, url, sums).items():
        written[f"winget/{name}"] = text
    for name, text in written.items():
        path = args.dir / "packaging" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
        print(path)


if __name__ == "__main__":
    main()
