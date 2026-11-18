#!/usr/bin/env python3
"""Lays out what each package manager's repository holds, from a release's
manifests.

A release carries `meridian-<version>-package-manifests.tar.gz`, written by
manifests.py: a Homebrew formula, a Scoop manifest and the winget manifests.
Each package manager wants those files at a particular place in a repository
of its own. Given the tarball (or the `packaging/` directory it unpacks to),
this writes under `<out>/`

    homebrew/Formula/meridian.rb      the tap repository, homebrew-meridian
    homebrew/README.md
    scoop/bucket/meridian.json        the bucket repository, scoop-meridian
    scoop/README.md
    winget/manifests/z/ZanattaMichael/Meridian/<version>/
        ZanattaMichael.Meridian.yaml, .installer.yaml, .locale.en-US.yaml
                                      the path winget-pkgs wants

Nothing here talks to GitHub. The `package managers` workflow runs this and
pushes each tree to its repository; by hand, copy each tree into its
repository and commit.

    python3 tools/release/package_managers.py \\
        --manifests dist/meridian-0.1.0-package-manifests.tar.gz --out trees

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import shutil
import sys
import tarfile
import tempfile

REPOSITORY = "ZanattaMichael/meridian-core"
OWNER = REPOSITORY.split("/")[0]
WINGET_ID = "ZanattaMichael.Meridian"
# winget-pkgs files a package under the first letter of its publisher, in
# lower case, then the publisher and the package as the identifier spells them.
WINGET_DIR = pathlib.Path("manifests") / OWNER[0].lower() / OWNER / "Meridian"

HOMEBREW_README = """# Homebrew tap for Meridian

```
brew install {owner_lower}/meridian/meridian
```

The formula in `Formula/meridian.rb` is written by the release pipeline of
[{repository}](https://github.com/{repository}) and replaced at each
release: do not edit it here. How a release is made, and how to check what
you downloaded, is in that repository's documentation.
"""

SCOOP_README = """# Scoop bucket for Meridian

```
scoop bucket add meridian https://github.com/{owner}/scoop-meridian
scoop install meridian
```

The manifest in `bucket/meridian.json` is written by the release pipeline of
[{repository}](https://github.com/{repository}) and replaced at each
release: do not edit it here. It carries `checkver` and `autoupdate`, so the
bucket can also keep itself current.
"""


def read_manifests(source: pathlib.Path, scratch: pathlib.Path) -> pathlib.Path:
    """Returns the directory holding homebrew/, scoop/ and winget/."""
    if source.is_dir():
        packaging = source
    else:
        with tarfile.open(source) as archive:
            for member in archive.getmembers():
                # The tarball is ours, but a release asset is still a download.
                if member.name.startswith(("/", "..")) or ".." in pathlib.PurePosixPath(member.name).parts:
                    sys.exit(f"{source}: refusing to unpack {member.name!r}")
                if not (member.isfile() or member.isdir()):
                    sys.exit(f"{source}: refusing to unpack {member.name!r}, which is not a file")
            # `data` refuses links and absolute paths too, where Python has it.
            if hasattr(tarfile, "data_filter"):
                archive.extractall(scratch, filter="data")
            else:
                archive.extractall(scratch)
        packaging = scratch / "packaging"
    for name in ("homebrew/meridian.rb", "scoop/meridian.json", f"winget/{WINGET_ID}.yaml"):
        if not (packaging / name).is_file():
            sys.exit(f"{source} does not hold {name}; it is not a release's package manifests")
    return packaging


def version_of(packaging: pathlib.Path) -> str:
    """The one version every manifest names."""
    formula = (packaging / "homebrew/meridian.rb").read_text(encoding="utf-8")
    found = re.search(r'^\s*version "([^"]+)"$', formula, re.MULTILINE)
    if not found:
        sys.exit("the formula names no version")
    version = found.group(1)
    scoop = json.loads((packaging / "scoop/meridian.json").read_text(encoding="utf-8"))
    winget = (packaging / f"winget/{WINGET_ID}.yaml").read_text(encoding="utf-8")
    if scoop.get("version") != version or f"PackageVersion: {version}\n" not in winget:
        sys.exit(f"the manifests do not agree on the version (formula says {version})")
    return version


def lay_out(packaging: pathlib.Path, out: pathlib.Path) -> dict[str, list[pathlib.Path]]:
    version = version_of(packaging)
    written: dict[str, list[pathlib.Path]] = {"homebrew": [], "scoop": [], "winget": []}

    def put(manager: str, source: pathlib.Path | None, target: pathlib.Path, text: str | None = None) -> None:
        path = out / manager / target
        path.parent.mkdir(parents=True, exist_ok=True)
        if text is not None:
            path.write_text(text, encoding="utf-8", newline="\n")
        else:
            assert source is not None
            shutil.copyfile(source, path)
        written[manager].append(path)

    put("homebrew", packaging / "homebrew/meridian.rb", pathlib.Path("Formula/meridian.rb"))
    put("homebrew", None, pathlib.Path("README.md"),
        HOMEBREW_README.format(owner_lower=OWNER.lower(), repository=REPOSITORY))
    put("scoop", packaging / "scoop/meridian.json", pathlib.Path("bucket/meridian.json"))
    put("scoop", None, pathlib.Path("README.md"), SCOOP_README.format(owner=OWNER, repository=REPOSITORY))
    for name in (f"{WINGET_ID}.yaml", f"{WINGET_ID}.installer.yaml", f"{WINGET_ID}.locale.en-US.yaml"):
        put("winget", packaging / "winget" / name, WINGET_DIR / version / name)
    (out / "VERSION").write_text(f"{version}\n", encoding="utf-8", newline="\n")
    return written


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifests", required=True, type=pathlib.Path,
                        help="the release's package-manifests tarball, or the packaging/ directory")
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        packaging = read_manifests(args.manifests, pathlib.Path(scratch))
        written = lay_out(packaging, args.out)
    for manager, paths in written.items():
        for path in paths:
            print(f"{manager}: {path}")


if __name__ == "__main__":
    main()
