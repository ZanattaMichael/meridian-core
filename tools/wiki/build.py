#!/usr/bin/env python3
"""Turn docs/ into the two published forms of the Meridian wiki.

    python3 tools/wiki/build.py site   # an mdBook source tree, for GitHub Pages
    python3 tools/wiki/build.py wiki   # flat pages, for the repository's GitHub Wiki
    python3 tools/wiki/build.py all    # both

docs/ is the only source. Nothing here is hand-kept per page: the navigation is
read from the directory tree and each page's first heading, the targets are
grouped by the category the generated capability matrix gives them, and the plugin
map is drawn from the same data. A page added under docs/ by any pull
request is therefore in the navigation, and a new plugin is in the plugin map,
the next time the pipeline runs.

Output goes to target/wiki/, which git ignores. Only the standard library is
used. docs/contributing/documentation-site.md describes the whole pipeline.
"""
from __future__ import annotations

import html
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / "docs"
HERE = Path(__file__).resolve().parent
OUT = ROOT / "target" / "wiki"

sys.path.insert(0, str(ROOT / "tools"))
from check_links import anchors as github_anchors  # noqa: E402

REPO = os.environ.get("GITHUB_REPOSITORY", "ZanattaMichael/meridian-core")
BRANCH = os.environ.get("WIKI_SOURCE_BRANCH", "main")
BLOB = f"https://github.com/{REPO}/blob/{BRANCH}/"
TREE = f"https://github.com/{REPO}/tree/{BRANCH}/"
WIKI_RAW = f"https://raw.githubusercontent.com/wiki/{REPO}/"

# Where generated graphics are written, relative to the published docs root.
GENERATED = "generated"

# ---------------------------------------------------------------------------
# Navigation
# ---------------------------------------------------------------------------

# Top-level sections in reading order. A directory that is not listed is placed
# after these and before the planning documents, in alphabetical order.
SECTION_ORDER = [
    "quickstart", "concepts", "tooling", "reference", "targets", "operations", "sdk",
    "troubleshooting", "contributing",
]
SECTION_LAST = ["plans", "branding"]
SECTION_TITLES = {
    "quickstart": "Quickstart",
    "concepts": "Concepts",
    "tooling": "Editor and authoring tools",
    "reference": "Reference",
    "targets": "Targets",
    "operations": "Operations",
    "sdk": "Plugin SDK",
    "troubleshooting": "Troubleshooting",
    "contributing": "Contributing",
    "plans": "Planning documents",
    "branding": "Branding",
}
# Pages whose order matters more than their file name.
PAGE_ORDER = {
    "quickstart": ["install", "first-resourceset", "first-two-stage", "workflows"],
    "concepts": ["ir-overview", "dependencies", "capability-model", "two-stage-architecture"],
    "tooling": ["language-server", "vscode", "other-editors", "formatter", "linter", "json-schema"],
    "reference": ["cli", "resource-types", "capability-matrix", "conformance-matrix", "lint"],
    "plans": ["design-plan", "testing-plan", "documentation-plan"],
}
INDEX_NAMES = ("index.md", "README.md")

# Categories as the plugins declare them, in the order the project lists them.
CATEGORIES = {
    "infrastructure-as-code": ("Infrastructure as code", "#6F9BFF"),
    "configuration-management": ("Configuration management", "#FF8A4C"),
    "containerization": ("Containers", "#35D0C2"),
    "container-orchestration": ("Orchestration", "#B48CFF"),
}
OTHER_COLOUR = "#9AA4C7"

FENCE = re.compile(r"(^|\n)(```|~~~).*?\n\2[^\n]*", re.DOTALL)
H1 = re.compile(r"^#\s+(.*?)\s*#*$", re.MULTILINE)
MATRIX_ROW = re.compile(r"^\|\s*\[([\w.-]+)\]\([^)]*\)\s*\|\s*([\w-]+)\s*\|", re.MULTILINE)


@dataclass
class Entry:
    """One line of navigation: a page, or a heading that only groups pages."""

    title: str
    page: Path | None = None  # path relative to docs/, None for a group heading
    children: list["Entry"] = field(default_factory=list)


def plain(text: str) -> str:
    """A heading as plain text: no code ticks, no link syntax, no emphasis."""
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"[`*]", "", text)
    return text.strip()


def page_title(rel: Path) -> str:
    body = FENCE.sub("", (DOCS / rel).read_text(encoding="utf-8"))
    found = H1.search(body)
    title = plain(found.group(1)) if found else rel.stem.replace("-", " ").capitalize()
    # "Target: `ansible`" reads better as "ansible" under a Targets heading.
    if rel.parts[0] == "targets" and title.lower().startswith("target:"):
        title = title.split(":", 1)[1].strip()
    return title


def target_categories() -> dict[str, str]:
    """target name -> category, from the generated capability matrix."""
    matrix = DOCS / "reference" / "capability-matrix.md"
    if not matrix.exists():
        return {}
    return dict(MATRIX_ROW.findall(matrix.read_text(encoding="utf-8")))


def category_label(category: str) -> tuple[str, str]:
    if category in CATEGORIES:
        return CATEGORIES[category]
    return category.replace("-", " ").capitalize(), OTHER_COLOUR


def grouped_targets() -> list[tuple[str, str, list[str]]]:
    """[(label, colour, [target, ...])] for every target that has a page."""
    cats = target_categories()
    names = sorted(p.stem for p in (DOCS / "targets").glob("*.md") if p.name not in INDEX_NAMES) \
        if (DOCS / "targets").is_dir() else []
    order = list(CATEGORIES) + sorted({c for c in cats.values() if c not in CATEGORIES})
    groups = []
    for category in order:
        members = [n for n in names if cats.get(n) == category]
        if members:
            groups.append((*category_label(category), members))
    rest = [n for n in names if n not in cats]
    if rest:
        groups.append(("Other", OTHER_COLOUR, rest))
    return groups


def ordered(directory: Path) -> list[Path]:
    """The pages directly in a directory, in reading order, index excluded."""
    rel_dir = directory.relative_to(DOCS)
    wanted = PAGE_ORDER.get(rel_dir.as_posix(), [])
    pages = [p for p in directory.glob("*.md") if p.name not in INDEX_NAMES]

    def key(p: Path):
        return (wanted.index(p.stem) if p.stem in wanted else len(wanted), p.stem)

    return sorted(pages, key=key)


def index_of(directory: Path) -> Path | None:
    for name in INDEX_NAMES:
        if (directory / name).exists():
            return directory / name
    return None


def entries_for(directory: Path) -> list[Entry]:
    out: list[Entry] = []
    index = index_of(directory)
    if index:
        rel = index.relative_to(DOCS)
        out.append(Entry(page_title(rel), rel))
    if directory == DOCS / "targets":
        for label, _, members in grouped_targets():
            group = Entry(label)
            for name in members:
                rel = Path("targets") / f"{name}.md"
                group.children.append(Entry(page_title(rel), rel))
            out.append(group)
    else:
        for page in ordered(directory):
            rel = page.relative_to(DOCS)
            out.append(Entry(page_title(rel), rel))
    for sub in sorted(d for d in directory.iterdir() if d.is_dir()):
        children = entries_for(sub)
        if not children:
            continue
        if children[0].page and children[0].page.name in INDEX_NAMES:
            head, rest = children[0], children[1:]
            head.children = rest
            out.append(head)
        else:
            out.append(Entry(sub.name.replace("-", " ").capitalize(), None, children))
    return out


def navigation() -> list[tuple[str, list[Entry]]]:
    """[(section title, entries)] for every top-level directory with pages."""
    dirs = {d.name: d for d in DOCS.iterdir() if d.is_dir()}
    middle = sorted(n for n in dirs if n not in SECTION_ORDER and n not in SECTION_LAST)
    sections = []
    for name in [*SECTION_ORDER, *middle, *SECTION_LAST]:
        if name not in dirs:
            continue
        entries = entries_for(dirs[name])
        if entries:
            title = SECTION_TITLES.get(name, name.replace("-", " ").capitalize())
            sections.append((title, entries))
    return sections


def all_pages() -> list[Path]:
    return sorted(p.relative_to(DOCS) for p in DOCS.rglob("*.md"))


# ---------------------------------------------------------------------------
# Graphics
# ---------------------------------------------------------------------------

FONT = "'Segoe UI', 'Helvetica Neue', Helvetica, Arial, sans-serif"
LOGOS = DOCS / "branding" / "logos"


def logo_inner(name: str) -> str | None:
    """The drawing inside a logo file, without its wrapper, title or metadata."""
    path = LOGOS / f"meridian-{name}.svg"
    if not path.exists():
        return None
    text = path.read_text(encoding="utf-8")
    text = re.sub(r"<metadata>.*?</metadata>", "", text, flags=re.DOTALL)
    text = re.sub(r"<title>.*?</title>", "", text, flags=re.DOTALL)
    found = re.search(r"<svg[^>]*>(.*)</svg>", text, flags=re.DOTALL)
    return found.group(1).strip() if found else None


def logo_at(name: str, x: float, y: float, size: float, colour: str = OTHER_COLOUR) -> str:
    """A plugin's logo placed in a larger drawing; a lettered tile if it has none."""
    inner = logo_inner(name)
    if inner is None:
        letter = html.escape(name[:1].upper())
        inner = (
            '<rect width="64" height="64" rx="14" fill="#16224F"/>'
            f'<text x="32" y="43" text-anchor="middle" font-family="{FONT}" font-size="30" '
            f'font-weight="700" fill="{colour}">{letter}</text>'
        )
    return f'<svg x="{x:g}" y="{y:g}" width="{size:g}" height="{size:g}" viewBox="0 0 64 64">{inner}</svg>'


def svg_document(width: int, height: int, label: str, body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" role="img" aria-label="{html.escape(label)}">'
        f"<title>{html.escape(label)}</title>"
        '<defs><linearGradient id="wiki-bg" x1="0" y1="0" x2="1" y2="1">'
        '<stop offset="0" stop-color="#22357A"/><stop offset="1" stop-color="#0A112B"/>'
        "</linearGradient></defs>"
        f'<rect width="{width}" height="{height}" rx="24" fill="url(#wiki-bg)"/>'
        f"{body}</svg>\n"
    )


def plugin_map_svg() -> str:
    """One document, the core, and every plugin under its category."""
    groups = grouped_targets() or [("Plugins", OTHER_COLOUR, [])]
    width = 1200
    margin, gap = 40, 20
    col_w = (width - 2 * margin - gap * (len(groups) - 1)) / len(groups)
    tile_h, tile_gap = 64, 12
    doc_y, core_y, bus_y, cols_y = 36, 132, 236, 270
    rows = max(len(members) for _, _, members in groups)
    col_h = 56 + rows * (tile_h + tile_gap) + 8
    height = int(cols_y + col_h + 64)
    mid = width / 2

    def box(x, y, w, h, fill, stroke, rx=14):
        return (f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="{rx}" '
                f'fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>')

    def text(x, y, value, size=20, weight=400, fill="#FFFFFF", anchor="middle"):
        return (f'<text x="{x:g}" y="{y:g}" text-anchor="{anchor}" font-family="{FONT}" '
                f'font-size="{size}" font-weight="{weight}" fill="{fill}">{html.escape(value)}</text>')

    def line(x1, y1, x2, y2):
        return (f'<path d="M{x1:g} {y1:g}L{x2:g} {y2:g}" stroke="#8FA3E8" stroke-width="2" '
                'stroke-linecap="round" fill="none"/>')

    parts = [
        box(mid - 230, doc_y, 460, 60, "#FFFFFF", "#FFFFFF", 30),
        text(mid, doc_y + 38, "One Meridian document", 22, 700, "#0A112B"),
        line(mid, doc_y + 60, mid, core_y),
        box(mid - 300, core_y, 600, 76, "#16224F", "#8FA3E8"),
        logo_at("core", mid - 284, core_y + 10, 56),
        text(mid + 28, core_y + 33, "meridian core", 22, 700),
        text(mid + 28, core_y + 58, "validate, compile, plan, apply", 17, 400, "#C9D4FF"),
        line(mid, core_y + 76, mid, bus_y),
    ]
    centres = [margin + i * (col_w + gap) + col_w / 2 for i in range(len(groups))]
    parts.append(line(centres[0], bus_y, centres[-1], bus_y))
    for centre, (label, colour, members) in zip(centres, groups):
        left = centre - col_w / 2
        parts.append(line(centre, bus_y, centre, cols_y))
        parts.append(box(left, cols_y, col_w, col_h, "#101A3F", colour))
        parts.append(f'<rect x="{left:g}" y="{cols_y:g}" width="{col_w:g}" height="6" rx="3" fill="{colour}"/>')
        parts.append(text(centre, cols_y + 36, label, 17 if len(groups) > 3 else 19, 700, colour))
        y = cols_y + 56
        for name in members:
            parts.append(box(left + 14, y, col_w - 28, tile_h, "#16224F", "#2B3A78", 12))
            parts.append(logo_at(name, left + 24, y + 8, 48, colour))
            parts.append(text(left + 86, y + 39, name, 19, 600, "#FFFFFF", "start"))
            y += tile_h + tile_gap
    parts.append(text(mid, height - 26,
                      "Each plugin compiles the document into its tool's own files, then drives that tool.",
                      17, 400, "#C9D4FF"))
    return svg_document(width, height, "Meridian plugins by category", "".join(parts))


def write_graphics(dest: Path) -> None:
    out = dest / GENERATED
    out.mkdir(parents=True, exist_ok=True)
    (out / "meridian-plugin-map.svg").write_text(plugin_map_svg(), encoding="utf-8")


# ---------------------------------------------------------------------------
# Page content
# ---------------------------------------------------------------------------

MD_LINK = re.compile(r"(!?\[[^\]]*\]\()([^)\s]+)(\s+\"[^\"]*\")?(\))")
HTML_ATTR = re.compile(r"(\b(?:src|srcset|href)=\")([^\"]+)(\")")
# Fenced blocks only: a link's own text is often in code ticks.
CODE = re.compile(r"(^|\n)(```|~~~).*?\n\2[^\n]*", re.DOTALL)
EXTERNAL = ("http://", "https://", "mailto:", "#")


def rewrite_links(text: str, rewrite) -> str:
    """Apply `rewrite(target) -> target` to every link outside fenced code."""

    def in_prose(chunk: str) -> str:
        chunk = MD_LINK.sub(lambda m: m.group(1) + rewrite(m.group(2)) + (m.group(3) or "") + m.group(4), chunk)
        return HTML_ATTR.sub(lambda m: m.group(1) + rewrite(m.group(2)) + m.group(3), chunk)

    out, last = [], 0
    for code in CODE.finditer(text):
        out.append(in_prose(text[last:code.start()]))
        out.append(code.group(0))
        last = code.end()
    out.append(in_prose(text[last:]))
    return "".join(out)


def resolve(rel: Path, target: str) -> tuple[Path | None, str]:
    """A link in docs/<rel> as (absolute path or None when external, '#anchor' or '')."""
    if target.startswith(EXTERNAL):
        return None, ""
    file_part, hash_, anchor = target.partition("#")
    path = Path(os.path.normpath((DOCS / rel).parent / file_part))
    return path, hash_ + anchor


def outside_url(path: Path) -> str:
    """A file in the repository but outside docs/ is linked on GitHub."""
    base = TREE if path.is_dir() else BLOB
    return base + path.relative_to(ROOT).as_posix()


def relative(from_rel: Path, to_rel: Path) -> str:
    return os.path.relpath(to_rel.as_posix(), from_rel.parent.as_posix() or ".").replace(os.sep, "/")


def decorate(rel: Path, text: str, image) -> str:
    """Add the graphics a published page carries and its source does not.

    `image(docs-relative path)` returns the address to use on this page.
    """
    if rel == Path("index.md"):
        plugin_map = (
            f'<p align="center"><img src="{image(Path(GENERATED) / "meridian-plugin-map.svg")}" '
            'alt="One Meridian document goes through the core to a plugin for each tool, grouped by category" '
            'width="100%"></p>\n\n'
        )
        first_section = re.search(r"^## ", text, flags=re.MULTILINE)
        cut = first_section.start() if first_section else len(text)
        return text[:cut] + plugin_map + text[cut:]
    if rel.parts[0] == "targets" and len(rel.parts) == 2:
        logo = Path("branding") / "logos" / f"meridian-{rel.stem}.svg"
        # A page that already shows its logo does not get a second one.
        if (DOCS / logo).exists() and logo.name not in text:
            return (f'<img src="{image(logo)}" alt="Meridian {rel.stem} plugin logo" '
                    f'width="72" align="right">\n\n' + text)
    return text


# ---------------------------------------------------------------------------
# The mdBook site
# ---------------------------------------------------------------------------

def site_link(rel: Path) -> Path:
    """What a link to a page must say on the site.

    mdBook publishes a directory's README.md as index.html but does not adjust
    links to it, so a link to README.md is written as a link to index.md. The
    file itself keeps its name, which keeps the page's "edit" link correct.
    """
    return rel.with_name("index.md") if rel.name == "README.md" else rel


def summary_lines(entries: list[Entry], depth: int = 0) -> list[str]:
    lines = []
    for entry in entries:
        link = entry.page.as_posix() if entry.page else ""
        lines.append(f"{'  ' * depth}- [{entry.title}]({link})")
        lines.extend(summary_lines(entry.children, depth + 1))
    return lines


def build_site() -> Path:
    book = OUT / "site"
    shutil.rmtree(book, ignore_errors=True)
    src = book / "docs"
    shutil.copytree(DOCS, src)
    shutil.copy(HERE / "book.toml", book / "book.toml")
    shutil.copytree(HERE / "theme", book / "theme")
    # mdBook uses these two files as the site's favicon.
    shutil.copy(LOGOS / "meridian-core.svg", book / "theme" / "favicon.svg")
    write_graphics(src)

    for rel in all_pages():
        def rewrite(target: str, rel: Path = rel) -> str:
            path, anchor = resolve(rel, target)
            if path is None or not path.exists():
                return target
            if DOCS not in path.parents and path != DOCS:
                return outside_url(path) + anchor
            to = path.relative_to(DOCS)
            if path.is_dir():
                index = index_of(path)
                if index is None:
                    return outside_url(path) + anchor
                to = index.relative_to(DOCS)
            return relative(rel, site_link(to)) + anchor

        text = rewrite_links((DOCS / rel).read_text(encoding="utf-8"), rewrite)
        text = decorate(rel, text, lambda p, rel=rel: relative(rel, p))
        (src / rel).write_text(text, encoding="utf-8")

    lines = ["# Summary", "", f"[{page_title(Path('index.md'))}](index.md)", ""]
    for title, entries in navigation():
        lines += [f"# {title}", "", *summary_lines(entries), ""]
    (src / "SUMMARY.md").write_text("\n".join(lines), encoding="utf-8")
    return book


# ---------------------------------------------------------------------------
# The GitHub Wiki
# ---------------------------------------------------------------------------

def wiki_name(rel: Path) -> str:
    """docs/targets/ansible.md -> Targets-Ansible; a directory's index takes its name."""
    if rel == Path("index.md"):
        return "Home"
    parts = list(rel.parts[:-1]) + ([] if rel.name in INDEX_NAMES else [rel.stem])
    return "-".join(p[:1].upper() + p[1:] for p in parts)


def sidebar_lines(entries: list[Entry], depth: int = 0) -> list[str]:
    lines = []
    for entry in entries:
        label = f"[{entry.title}]({wiki_name(entry.page)})" if entry.page else entry.title
        lines.append(f"{'  ' * depth}- {label}")
        lines.extend(sidebar_lines(entry.children, depth + 1))
    return lines


def source_commit() -> str:
    sha = os.environ.get("GITHUB_SHA")
    if not sha:
        done = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True)
        sha = done.stdout.strip() if done.returncode == 0 else ""
    return sha


def build_wiki() -> Path:
    wiki = OUT / "wiki"
    shutil.rmtree(wiki, ignore_errors=True)
    wiki.mkdir(parents=True)
    write_graphics(wiki)

    pages = all_pages()
    names = {rel: wiki_name(rel) for rel in pages}
    if len(set(names.values())) != len(names):
        clash = sorted(n for n in set(names.values()) if list(names.values()).count(n) > 1)
        raise SystemExit(f"two pages would get the same wiki name: {clash}")
    problems: list[str] = []

    def asset(docs_rel: Path) -> str:
        """Copy a non-page file into the wiki and return its raw address."""
        source = DOCS / docs_rel
        if source.exists():
            (wiki / docs_rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(source, wiki / docs_rel)
        return WIKI_RAW + docs_rel.as_posix()

    for rel in pages:
        def rewrite(target: str, rel: Path = rel) -> str:
            path, anchor = resolve(rel, target)
            if path is None or not path.exists():
                return target
            if DOCS not in path.parents and path != DOCS:
                return outside_url(path) + anchor
            if path.is_dir():
                index = index_of(path)
                if index is None:
                    return outside_url(path) + anchor
                path = index
            to = path.relative_to(DOCS)
            if to not in names:
                return asset(to)
            if anchor and anchor[1:].lower() not in github_anchors(path):
                problems.append(f"docs/{rel}: no heading for {target}")
            return names[to] + anchor

        text = rewrite_links((DOCS / rel).read_text(encoding="utf-8"), rewrite)
        text = decorate(rel, text, asset)
        note = f"<!-- Generated from docs/{rel.as_posix()}. Edit that file, not this page. -->\n"
        (wiki / f"{names[rel]}.md").write_text(note + text, encoding="utf-8")

    sidebar = [
        f'<p align="center"><a href="Home"><img src="{asset(Path("branding/logos/meridian-core.svg"))}" '
        'alt="Meridian" width="72"></a></p>',
        "",
        "**[Meridian](Home)**",
        "",
    ]
    for title, entries in navigation():
        sidebar += [f"**{title}**", "", *sidebar_lines(entries), ""]
    (wiki / "_Sidebar.md").write_text("\n".join(sidebar), encoding="utf-8")

    sha = source_commit()
    at = f" at [`{sha[:7]}`](https://github.com/{REPO}/commit/{sha})" if sha else ""
    (wiki / "_Footer.md").write_text(
        f"This wiki is generated from [`docs/`]({TREE}docs){at} by the "
        f"[wiki pipeline]({BLOB}.github/workflows/wiki.yml). "
        "Changes made here are overwritten; open a pull request against `docs/` instead.\n",
        encoding="utf-8",
    )
    if problems:
        raise SystemExit("\n".join(problems))
    return wiki


def main(argv: list[str]) -> int:
    what = argv[1] if len(argv) > 1 else "all"
    if what not in ("site", "wiki", "all"):
        print(__doc__, file=sys.stderr)
        return 2
    if what in ("site", "all"):
        print(f"site source: {build_site().relative_to(ROOT)}")
    if what in ("wiki", "all"):
        print(f"wiki pages:  {build_wiki().relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
