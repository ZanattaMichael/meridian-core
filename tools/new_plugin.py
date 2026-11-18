#!/usr/bin/env python3
"""Start a new Meridian plugin from the template crate.

    python3 tools/new_plugin.py nomad --category container-orchestration

copies crates/meridian-plugin-template to plugins/nomad, renames everything in
it, moves its example to examples/nomad/, adds the crate to the workspace and
writes a stub of the target's documentation page. The result builds and its
tests pass before you have changed a line, so every later failure is yours.

What to do next is in docs/contributing/writing-a-plugin.md.
"""
from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "crates" / "meridian-plugin-template"

CATEGORIES = {
    "infrastructure-as-code": "InfrastructureAsCode",
    "configuration-management": "ConfigurationManagement",
    "containerization": "Containerization",
    "container-orchestration": "ContainerOrchestration",
}

# The categories whose targets create things instead of configuring a host.
# Their documents are `kind: Infrastructure`; the core refuses a ResourceSet.
INFRASTRUCTURE_ONLY = ("infrastructure-as-code", "container-orchestration")

# Where the template's own introduction ends in src/lib.rs.
TOOL_HEADING = "//! # The tool this template drives"

DOC_PAGE = """\
# Target: `{name}`

TODO(plugin): one sentence on what Meridian compiles a document to for this
tool, and which command runs it.

## What it does and does not support

TODO(plugin): open with this, before anything else. List what a document can
ask of this target, and just as plainly what it refuses and why.

| | |
|---|---|
| Category | {category} |
| Compiles to | TODO(plugin) |
| Runs with | TODO(plugin) |
| Resource types | `file` |
| `notifies` | refused |
| `runtimeWhen` | refused |
| Outputs | none |

## What you need installed

TODO(plugin): the tool, the versions you tested, and the environment variable
that names the program when it is not on `PATH` (`{variable}`).

## A worked example

The document is [`examples/{name}/site.yaml`](../../examples/{name}/site.yaml),
which a test compiles with the real plugin.

TODO(plugin): show the document, the files it compiles to, and what `meridian
plan` and `meridian apply` print.

## Refusals

TODO(plugin): what the plugin refuses, under which rule, what triggers it,
and what the author should do instead. The rules are the general ones in
docs/reference/refusal-rules.md unless the plugin declares its own.
"""


def fail(message: str) -> None:
    print(f"new_plugin: {message}", file=sys.stderr)
    raise SystemExit(1)


def rename(text: str, name: str) -> str:
    snake = name.replace("-", "_")
    camel = "".join(part.capitalize() for part in name.split("-"))
    text = text.replace("meridian-plugin-template", f"meridian-target-{name}")
    text = text.replace("meridian-target-template", f"meridian-target-{name}")
    text = text.replace("meridian_plugin_template", f"meridian_target_{snake}")
    text = text.replace("MERIDIAN_TEMPLATE", f"MERIDIAN_{snake.upper()}")
    text = re.sub(r"\bTemplateRunner\b", f"{camel}Runner", text)
    text = re.sub(r"\bTemplate\b", camel, text)
    text = text.replace('"template"', f'"{name}"')
    text = text.replace("target: template", f"target: {name}")
    text = text.replace("the template plugin", f"the {name} plugin")
    # A recorded refusal names the target that refused.
    text = text.replace(": template: ", f": {name}: ")
    return text


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("name", help="the target name documents will select, for example nomad")
    parser.add_argument(
        "--category",
        choices=sorted(CATEGORIES),
        default="configuration-management",
        help="the kind of tool the plugin drives (default: %(default)s)",
    )
    args = parser.parse_args()
    name = args.name

    if not re.fullmatch(r"[a-z][a-z0-9]*(-[a-z0-9]+)*", name):
        fail(f"{name!r} is not a target name: use lower-case letters, digits and single hyphens")
    if name in ("template", "fake"):
        fail(f"{name!r} is taken by a crate the tests use")
    crate = ROOT / "plugins" / name
    examples = ROOT / "examples" / name
    page = ROOT / "docs" / "targets" / f"{name}.md"
    for path in (crate, examples, page):
        if path.exists():
            fail(f"{path.relative_to(ROOT)} already exists")

    shutil.copytree(TEMPLATE, crate, ignore=shutil.ignore_patterns("example", "README.md"))
    for path in sorted(p for p in crate.rglob("*") if p.is_file()):
        path.write_text(rename(path.read_text(encoding="utf-8"), name), encoding="utf-8")

    # A plugin's package and binary share a name, so it needs no [[bin]]
    # section, and unlike the template it is published.
    manifest = crate / "Cargo.toml"
    text = manifest.read_text(encoding="utf-8")
    text = re.sub(r"\[\[bin\]\]\n(?:.+\n)+\n", "", text)
    text = text.replace("publish = false\n", "")
    text = re.sub(
        r'description = ".*"',
        f'description = "The Meridian plugin for {name}: TODO(plugin) say what it compiles to and runs"',
        text,
    )
    text = text.replace('path = "../', 'path = "../../crates/')
    manifest.write_text(text, encoding="utf-8")

    # The crate's opening words describe the template; the plugin gets its own.
    lib = crate / "src" / "lib.rs"
    text = lib.read_text(encoding="utf-8")
    start = text.index(TOOL_HEADING)
    text = (
        f"//! Compiles a Meridian document for {name} and runs it.\n"
        "//!\n"
        "//! TODO(plugin): say what the document compiles to and which command runs\n"
        "//! it. This crate started as a copy of the template, so what follows still\n"
        "//! describes the template's imaginary tool until you replace it.\n"
        "//!\n" + text[start:].replace(TOOL_HEADING, "//! # The tool", 1)
    )
    text = text.replace(
        "Category::ConfigurationManagement", f"Category::{CATEGORIES[args.category]}"
    )
    lib.write_text(text, encoding="utf-8")

    # The example lives with the others, where a test compiles every one of
    # them through the real command and the real plugin.
    examples.mkdir(parents=True)
    example = rename((TEMPLATE / "example" / "site.yaml").read_text(encoding="utf-8"), name)
    if args.category in INFRASTRUCTURE_ONLY:
        # A target of this category takes Infrastructure documents and refuses
        # a ResourceSet, so the example becomes one: no host to configure.
        example = example.replace("kind: ResourceSet", "kind: Infrastructure")
        example, hosts = re.subn(r"  targetHost:\n(?:    .*\n)+", "", example)
        if hosts != 1 or "kind: Infrastructure" not in example:
            fail("could not turn the template's example into an Infrastructure document")
    (examples / "site.yaml").write_text(example, encoding="utf-8")
    tests = crate / "tests" / "compile.rs"
    text = tests.read_text(encoding="utf-8")
    text = text.replace(f"crates/meridian-target-{name}/example/site.yaml", f"examples/{name}/site.yaml")
    tests.write_text(text, encoding="utf-8")

    page.write_text(
        DOC_PAGE.format(
            name=name,
            category=args.category.replace("-", " "),
            variable=f"MERIDIAN_{name.replace('-', '_').upper()}",
        ),
        encoding="utf-8",
    )

    # The workspace lists `plugins/*`, so the crate is a member already. The one
    # list of targets is `crates/meridian-all-targets`, whose tests fail until
    # the plugin is in it and where readers look for it.

    # Renaming changes how the imports sort, so the crate is formatted again.
    # Without cargo the copy still builds; `cargo fmt --all` tidies it later.
    try:
        subprocess.run(
            ["cargo", "fmt", "-p", f"meridian-target-{name}"], cwd=ROOT, check=False
        )
    except OSError:
        pass

    print(f"""Created the {name} plugin:

  plugins/{name}/               the crate (binary: meridian-target-{name})
  examples/{name}/site.yaml     its example document
  docs/targets/{name}.md        its documentation page, as a stub

Add it to the one list of targets, which the conformance tests and the
capability matrix read:

  crates/meridian-all-targets/Cargo.toml    a dependency on meridian-target-{name}
  crates/meridian-all-targets/src/lib.rs    a target!(...) entry in ALL, in name order

Check that it works before changing anything:

  cargo test -p meridian-target-{name}
  cargo build -p meridian-cli -p meridian-target-{name}
  target/debug/meridian compile examples/{name}/site.yaml --out build

Then follow docs/contributing/writing-a-plugin.md from step 3. Search for
TODO(plugin) to find every decision that is yours to make.""")
    return 0


if __name__ == "__main__":
    sys.exit(main())
