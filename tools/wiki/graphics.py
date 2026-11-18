#!/usr/bin/env python3
"""Draw the explanatory graphics under docs/graphics/ and the animated marks.

    python3 tools/wiki/graphics.py          # write every file
    python3 tools/wiki/graphics.py --check  # fail when a file on disk differs

The files are committed, so GitHub shows them in the Markdown under docs/ as
well as the site and the Wiki. They are never drawn by hand: this script is
their only source, and the wiki workflow runs it with --check so that an edit
to a graphic is an edit here. Only the standard library is used.

Every graphic shares one look, the Meridian tile (docs/branding/README.md),
and one motion: a point of light travelling along the lines that join things,
the way the banner's meridians leave the single point that stands for the DSL.
The motion is CSS inside each SVG, so it plays wherever the file is shown as
an image, and `prefers-reduced-motion: reduce` turns it off and leaves the
still drawing.

Two further files are derived from the branding, not drawn here from scratch:
the animated core mark and the animated banners, which take the still files
and add the travelling light to their meridians.
"""
from __future__ import annotations

import html
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / "docs"
GRAPHICS = DOCS / "graphics"
BRANDING = DOCS / "branding"

FONT = "'Segoe UI', 'Helvetica Neue', Helvetica, Arial, sans-serif"
MONO = "ui-monospace, 'Cascadia Mono', 'SF Mono', Menlo, Consolas, monospace"

# The palette from docs/branding/README.md.
IAC, CM, CON, ORC, PIPE = "#6F9BFF", "#FF8A4C", "#35D0C2", "#B48CFF", "#FFC857"
WHITE, TEXT_SOFT, LINE = "#FFFFFF", "#C9D4FF", "#8FA3E8"
TILE, TILE_EDGE, WELL = "#16224F", "#2B3A78", "#101A3F"
RED, GREEN = "#FF7A90", "#7EE2A8"

# ---------------------------------------------------------------------------
# Motion
# ---------------------------------------------------------------------------

STYLE = """
.m-flow{stroke-dasharray:12 88;stroke-dashoffset:0;animation:m-travel var(--m-time,3s) linear infinite}
.m-flow.m-back{animation-direction:reverse}
.m-breathe{animation:m-breathe 3.2s ease-in-out infinite}
@keyframes m-travel{to{stroke-dashoffset:-100}}
@keyframes m-breathe{0%,100%{opacity:.45}50%{opacity:1}}
@media (prefers-reduced-motion:reduce){.m-flow{display:none}.m-breathe{animation:none;opacity:.85}}
""".strip()


def esc(value: str) -> str:
    return html.escape(value, quote=True)


def doc(width: int, height: int, label: str, body: str) -> str:
    """One graphic: the tile gradient, the motion styles, and the drawing."""
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" role="img" aria-label="{esc(label)}">\n'
        f"<title>{esc(label)}</title>\n"
        f"<style>{STYLE}</style>\n"
        "<defs>\n"
        '<linearGradient id="g-bg" x1="0" y1="0" x2="1" y2="1">'
        '<stop offset="0" stop-color="#22357A"/><stop offset="1" stop-color="#0A112B"/></linearGradient>\n'
        '<marker id="g-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
        f'<path d="M0 0L10 5L0 10z" fill="{LINE}"/></marker>\n'
        "</defs>\n"
        f'<rect width="{width}" height="{height}" rx="24" fill="url(#g-bg)"/>\n'
        f"{body}\n</svg>\n"
    )


def text(x: float, y: float, value: str, size: float = 16, weight: int = 400, fill: str = WHITE,
         anchor: str = "middle", mono: bool = False, opacity: float | None = None) -> str:
    extra = f' opacity="{opacity}"' if opacity is not None else ""
    font = MONO if mono else FONT
    # Leading spaces are indentation in a code-like line; keep them.
    stripped = value.lstrip(" ")
    value = "\u00a0" * (len(value) - len(stripped)) + stripped
    return (f'<text x="{x:g}" y="{y:g}" text-anchor="{anchor}" font-family="{font}" '
            f'font-size="{size:g}" font-weight="{weight}" fill="{fill}"{extra}>{esc(value)}</text>')


def lines(x: float, y: float, rows: list[str], size: float = 14, fill: str = TEXT_SOFT,
          anchor: str = "middle", gap: float = 1.35, mono: bool = False) -> str:
    """Several lines of text, the first at y."""
    return "".join(text(x, y + i * size * gap, row, size, 400, fill, anchor, mono) for i, row in enumerate(rows))


def rect(x: float, y: float, w: float, h: float, fill: str = TILE, stroke: str = TILE_EDGE,
         rx: float = 14, width: float = 1.5, dash: str | None = None, opacity: float | None = None) -> str:
    extra = f' stroke-dasharray="{dash}"' if dash else ""
    extra += f' opacity="{opacity}"' if opacity is not None else ""
    return (f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="{rx:g}" fill="{fill}" '
            f'stroke="{stroke}" stroke-width="{width}"{extra}/>')


def card(x: float, y: float, w: float, h: float, title: str, body: list[str] | None = None,
         colour: str = LINE, title_size: float = 18, body_size: float = 13.5, mono_body: bool = False,
         fill: str = TILE, band: bool = True) -> str:
    """A tile with a coloured band on top, a title and some lines under it."""
    parts = [rect(x, y, w, h, fill, colour)]
    if band:
        parts.append(f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="5" rx="2.5" fill="{colour}"/>')
    parts.append(text(x + w / 2, y + 30, title, title_size, 700, WHITE))
    if body:
        parts.append(lines(x + w / 2, y + 52, body, body_size, TEXT_SOFT, mono=mono_body))
    return "".join(parts)


def chip(x: float, y: float, w: float, label: str, colour: str = LINE, h: float = 28,
         size: float = 13, mono: bool = False, fill: str = WELL) -> str:
    return (rect(x, y, w, h, fill, colour, h / 2, 1.2)
            + text(x + w / 2, y + h / 2 + size * 0.36, label, size, 600, WHITE, mono=mono))


def flow(d: str, colour: str = LINE, arrow: bool = True, back: bool = False, delay: float = 0,
         seconds: float = 3, width: float = 2) -> str:
    """A connector: a still line, and a point of light travelling along it.

    `pathLength="100"` makes the dash pattern a percentage of the path, so the
    same style animates a short line and a long one at the same pace.
    """
    marker = ' marker-end="url(#g-arrow)"' if arrow else ""
    cls = "m-flow m-back" if back else "m-flow"
    style = f"--m-time:{seconds:g}s;animation-delay:{-delay:g}s"
    return (
        f'<path d="{d}" fill="none" stroke="{LINE}" stroke-width="{width:g}" stroke-linecap="round"{marker}/>'
        f'<path d="{d}" fill="none" stroke="{colour}" stroke-width="{width + 4:g}" stroke-linecap="round" '
        f'opacity=".45" pathLength="100" class="{cls}" style="{style}"/>'
        f'<path d="{d}" fill="none" stroke="{WHITE}" stroke-width="{width + 0.5:g}" stroke-linecap="round" '
        f'pathLength="100" class="{cls}" style="{style}"/>'
    )


def point(x: float, y: float, r: float = 6, colour: str = WHITE) -> str:
    """The single point: the DSL, breathing."""
    return (f'<circle cx="{x:g}" cy="{y:g}" r="{r * 2.2:g}" fill="{colour}" opacity=".18" class="m-breathe"/>'
            f'<circle cx="{x:g}" cy="{y:g}" r="{r:g}" fill="{colour}"/>')


def caption(width: int, height: int, value: str) -> str:
    return text(width / 2, height - 24, value, 15, 400, TEXT_SOFT)


def heading(x: float, y: float, value: str, colour: str = LINE, anchor: str = "start") -> str:
    return text(x, y, value.upper(), 12.5, 700, colour, anchor)


# ---------------------------------------------------------------------------
# The graphics
# ---------------------------------------------------------------------------

CATEGORIES = [
    ("Infrastructure as code", IAC, ["terraform", "opentofu", "cloudformation", "bicep", "arm", "pulumi", "packer"],
     ["terraform", "tofu", "aws", "az", "pulumi", "packer"]),
    ("Configuration management", CM, ["ansible", "puppet", "openvox", "chef", "salt", "dsc", "cloud-init"],
     ["ansible-playbook", "puppet apply", "chef-client", "salt-call", "Invoke-DscRunner"]),
    ("Containers", CON, ["docker", "podman"], ["docker compose", "podman", "systemctl"]),
    ("Orchestration", ORC, ["kubernetes", "helm"], ["kubectl apply", "helm upgrade --install"]),
    ("Pipelines", PIPE, ["github-actions", "azure-pipelines", "gitlab-ci", "buildkite", "argo-workflows", "octopus"],
     ["a workflow file", "the engine runs meridian"]),
]


def wrap(words: list[str], limit: int) -> list[str]:
    """Join comma-separated names into lines of at most `limit` characters."""
    rows: list[str] = []
    for word in words:
        if rows and len(rows[-1]) + 2 + len(word) <= limit:
            rows[-1] += ", " + word
        else:
            rows.append(word)
    return rows


def gutter(y: float, h: float, label: str, colour: str = LINE) -> str:
    """A section's name, turned on its side in the left margin."""
    cy = y + h / 2
    return (f'<text x="34" y="{cy:g}" text-anchor="middle" font-family="{FONT}" font-size="12" font-weight="700" '
            f'letter-spacing="1.5" fill="{colour}" transform="rotate(-90 34 {cy:g})">{esc(label.upper())}</text>')


def chip_row(x: float, y: float, labels: list[str], colour: str = LINE, size: float = 12.5) -> str:
    out = []
    for label in labels:
        w = round(size * 0.53 * len(label) + 24)
        out.append(chip(x, y, w, label, colour, 26, size))
        x += w + 10
    return "".join(out)


def architecture() -> str:
    W, H = 1200, 780
    L, R = 90, 1160  # the drawing, right of the gutter
    p: list[str] = []
    # What you write.
    top, card_h = 48, 92
    p.append(gutter(top, card_h, "You write", WHITE))
    cards = [
        ("Documents", ["kind: Infrastructure, ResourceSet or Workflow", "YAML or JSON, one tool each"], 268),
        ("Data and rules", ["--data layers decide every when", "compile rules say what a layer may set"], 268),
        ("Modules", ["meridian.yaml names what you need", "meridian.lock pins what was fetched"], 268),
        ("Editor tools", ["lsp, mcp, fmt, lint, schema:", "the same compiler"], 206),
    ]
    x = L
    centres = []
    for title, body, w in cards:
        p.append(card(x, top, w, card_h, title, body, WHITE, 17, 12.5))
        centres.append(x + w / 2)
        x += w + 20
    # The core.
    core_y, core_h = 196, 150
    p.append(gutter(core_y, core_h, "The core", WHITE))
    p.append(rect(L, core_y, R - L, core_h, WELL, LINE, 18))
    p.append(text(L + 28, core_y + 36, "meridian", 24, 700, WHITE, "start"))
    p.append(text(L + 142, core_y + 36, "knows the language, and nothing about any tool", 14, 400, TEXT_SOFT, "start"))
    p.append(text(R - 28, core_y + 36, "validate · compile · plan · apply · import · module · secret · rules", 13, 400, TEXT_SOFT, "end", mono=True))
    p.append(chip_row(L + 28, core_y + 56, [
        "parse and validate", "expressions, conditions", "data layers", "compile rules",
        "order by dependsOn", "stages and outputs", "lock, record, resume",
    ]))
    p.append(chip_row(L + 28, core_y + 96, [
        "locked-memory secrets", "policy checks", "reports, sinks", "plugin signatures",
        "rollback, drift", "import", "adopt", "retarget", "schema sync",
    ], TEXT_SOFT))
    for i, cx in enumerate(centres):
        p.append(flow(f"M{cx:g} {top + card_h}L{cx:g} {core_y}", WHITE if i < 3 else TEXT_SOFT, delay=i * 0.9, seconds=3 if i < 3 else 4))
    # The protocol.
    proto_y, proto_h = 400, 58
    p.append(gutter(proto_y - 20, proto_h + 40, "Protocol"))
    p.append(rect(L, proto_y, R - L, proto_h, TILE, LINE, 29))
    mid = (L + R) / 2
    p.append(text(mid, proto_y + 25, "one plugin program per target, spoken to over a private socket", 15, 600, WHITE))
    p.append(text(mid, proto_y + 45, "Describe · Emit · Plan · Apply · Outputs · Identifiers · Import · ExportSchemas", 13, 400, TEXT_SOFT, mono=True))
    p.append(flow(f"M{mid - 40:g} {core_y + core_h}L{mid - 40:g} {proto_y}", WHITE, delay=0.4))
    p.append(flow(f"M{mid + 40:g} {proto_y}L{mid + 40:g} {core_y + core_h}", LINE, delay=1.6))
    p.append(text(mid - 56, core_y + core_h + 22, "ordered resources, data, secrets", 12.5, 400, TEXT_SOFT, "end"))
    p.append(text(mid + 56, core_y + core_h + 22, "files, plans, events, outputs", 12.5, 400, TEXT_SOFT, "start"))
    p.append(text(mid - 56, core_y + core_h + 38, "down", 12.5, 400, TEXT_SOFT, "end"))
    p.append(text(mid + 56, core_y + core_h + 38, "back", 12.5, 400, TEXT_SOFT, "start"))
    # Plugins by category.
    cat_y, cat_h = 500, 112
    p.append(gutter(cat_y, cat_h, "Plugins"))
    gap = 20
    col_w = (R - L - (len(CATEGORIES) - 1) * gap) / len(CATEGORIES)
    tool_y, tool_h = 650, 84
    p.append(gutter(tool_y, tool_h, "Tools"))
    for i, (label, colour, targets, tools) in enumerate(CATEGORIES):
        x = L + i * (col_w + gap)
        cx = x + col_w / 2
        p.append(rect(x, cat_y, col_w, cat_h, TILE, colour))
        p.append(f'<rect x="{x:g}" y="{cat_y}" width="{col_w:g}" height="5" rx="2.5" fill="{colour}"/>')
        p.append(text(cx, cat_y + 30, label, 14, 700, colour))
        p.append(lines(cx, cat_y + 54, wrap(targets, 24), 12, WHITE, mono=True, gap=1.3))
        p.append(flow(f"M{cx:g} {proto_y + proto_h}L{cx:g} {cat_y}", colour, delay=i * 0.7))
        p.append(rect(x, tool_y, col_w, tool_h, WELL, TILE_EDGE, 12, 1.2, "4 4"))
        p.append(lines(cx, tool_y + 24, wrap(tools, 26), 11.5, TEXT_SOFT, mono=True))
        p.append(flow(f"M{cx:g} {cat_y + cat_h}L{cx:g} {tool_y}", colour, delay=i * 0.7 + 1.5))
    p.append(caption(W, H, "A plugin compiles the document into its tool's own files, then drives that tool, as you would by hand. Nothing in the core knows how."))
    return doc(W, H, "Meridian architecture: documents into the core, the core to a plugin, the plugin to its tool", "".join(p))


def compile_pipeline() -> str:
    W, H = 1200, 470
    steps = [
        ("Read", ["files, as", "YAML or JSON"]),
        ("Decode", ["typed documents;", "unknown fields", "are errors"]),
        ("Validate", ["shape, links,", "no cycles"]),
        ("Stage", ["Infrastructure", "first, then", "ResourceSets"]),
        ("Target", ["plugin found;", "category and", "targetVersion fit"]),
        ("Order", ["every when", "decided; sorted", "by dependsOn"]),
        ("Compile", ["the plugin writes", "the tool's", "own files"]),
        ("Run", ["the tool plans,", "or applies"]),
        ("Hand on", ["outputs reach", "the stages", "that asked"]),
    ]
    p: list[str] = []
    n = len(steps)
    gap = 14
    w = (W - 80 - gap * (n - 1)) / n
    y, h = 110, 118
    xs = []
    for i, (title, body) in enumerate(steps):
        x = 40 + i * (w + gap)
        xs.append(x)
        colour = WHITE if i < 6 else (IAC if i == 6 else (CM if i == 7 else CON))
        p.append(rect(x, y, w, h, TILE, colour))
        p.append(f'<circle cx="{x + 22:g}" cy="{y + 24:g}" r="13" fill="{colour}"/>')
        p.append(text(x + 22, y + 29, str(i + 1), 14, 800, "#0A112B"))
        p.append(text(x + 42, y + 29, title, 15, 700, WHITE, "start"))
        p.append(lines(x + w / 2, y + 60, body, 12, TEXT_SOFT, gap=1.3))
        if i:
            p.append(flow(f"M{x - gap:g} {y + h / 2:g}L{x:g} {y + h / 2:g}", WHITE, delay=i * 0.33, seconds=1.4))
    # Outputs go back to the stages (step 4) that asked for them.
    last_mid = xs[-1] + w / 2
    stage_mid = xs[3] + w / 2
    p.append(flow(f"M{last_mid:g} {y}C{last_mid:g} 56 {stage_mid:g} 56 {stage_mid:g} {y}", CON, delay=0.8, seconds=4))
    p.append(text((last_mid + stage_mid) / 2, 44, "an Infrastructure stage's outputs become a later stage's targetHost", 13, 400, TEXT_SOFT))
    # Who gets how far.
    p.append(heading(40, 268, "How far each command goes"))
    bars = [("meridian validate", 3, WHITE, "no plugin is loaded: the language only"),
            ("meridian compile", 7, IAC, "writes each target's files, changes nothing"),
            ("meridian plan", 8, CM, "asks each tool what applying would change"),
            ("meridian apply", 9, CON, "makes it so, and carries outputs on")]
    for j, (name, upto, colour, note) in enumerate(bars):
        by = 282 + j * 36
        end = xs[upto - 1] + w
        p.append(rect(40, by, end - 40, 26, WELL, colour, 13, 1.2))
        p.append(f'<rect x="40" y="{by}" width="{end - 40:g}" height="26" rx="13" fill="{colour}" opacity=".12"/>')
        p.append(text(56, by + 18, name, 13.5, 700, WHITE, "start", mono=True))
        note = f"through step {upto}: {note}"
        if end - 40 > 700:
            p.append(text(end - 14, by + 18, note, 12.5, 400, TEXT_SOFT, "end"))
        else:
            p.append(text(end + 14, by + 18, note, 12.5, 400, TEXT_SOFT, "start"))
    p.append(caption(W, H, "Step 7 is where a target refuses what its tool cannot express. It never writes files that quietly do less."))
    return doc(W, H, "The path of a document: nine steps from a file to a tool that has run, and how far each command goes", "".join(p))


def two_stage() -> str:
    W, H = 1200, 500
    p: list[str] = []
    p.append(heading(40, 40, "Stage 1: what should exist?", IAC))
    p.append(rect(40, 52, 420, 250, TILE, IAC))
    p.append(f'<rect x="40" y="52" width="420" height="5" rx="2.5" fill="{IAC}"/>')
    p.append(text(250, 84, "kind: Infrastructure", 18, 700, WHITE, mono=True))
    p.append(lines(62, 116, [
        "spec:",
        "  target: terraform",
        "  resources:",
        "    - id: net     type: aws_vpc",
        "    - id: web     type: aws_instance",
        "  outputs:",
        "    - name: address",
        "      value: ${web.public_ip}",
    ], 13.5, TEXT_SOFT, "start", mono=True))
    p.append(chip(62, 258, 376, "terraform apply creates the machine and knows its address", IAC, 30, 12.5))

    p.append(heading(740, 40, "Stage 2: how should that machine be set up?", CM))
    p.append(rect(740, 52, 420, 250, TILE, CM))
    p.append(f'<rect x="740" y="52" width="420" height="5" rx="2.5" fill="{CM}"/>')
    p.append(text(950, 84, "kind: ResourceSet", 18, 700, WHITE, mono=True))
    p.append(lines(762, 116, [
        "spec:",
        "  target: ansible",
        "  targetHost: ${infra.address}",
        "  resources:",
        "    - id: nginx   type: package",
        "    - id: site    type: file",
        "    - id: svc     type: service",
        "      notifies: [nginx]",
    ], 13.5, TEXT_SOFT, "start", mono=True))
    p.append(chip(762, 258, 376, "ansible-playbook logs in to that address and converges it", CM, 30, 12.5))

    # The hand-off.
    p.append(flow("M460 177L740 177", WHITE, delay=0, seconds=2.4, width=2.5))
    p.append(rect(500, 150, 200, 54, WELL, WHITE, 27, 1.2))
    p.append(text(600, 172, "one declared value", 13, 700, WHITE))
    p.append(text(600, 191, "address = 203.0.113.7", 12.5, 400, TEXT_SOFT, mono=True))
    # Timeline.
    ty = 350
    p.append(heading(40, ty - 12, "In one meridian apply"))
    p.append(f'<path d="M40 {ty + 28}H1160" stroke="{TILE_EDGE}" stroke-width="2"/>')
    marks = [(40, "stage 1 compiled", IAC), (300, "terraform runs to the end", IAC), (560, "the output is read back from the tool", WHITE),
             (820, "stage 2 compiled with the value", CM), (1080, "ansible runs", CM)]
    for i, (x, label, colour) in enumerate(marks):
        p.append(point(x + 2, ty + 28, 5, colour))
        p.append(text(x + 2, ty + 56, label, 12.5, 400, TEXT_SOFT, "start" if i == 0 else ("end" if i == len(marks) - 1 else "middle")))
        if i:
            p.append(flow(f"M{marks[i - 1][0] + 2} {ty + 28}L{x + 2} {ty + 28}", colour, arrow=False, delay=i * 0.6, seconds=2.4))
    p.append(caption(W, H, "Meridian stores no output. It reads each one back from the tool that produced it, and a ResourceSet aimed at Terraform is refused at compile."))
    return doc(W, H, "The two-stage architecture: an Infrastructure stage creates the machine and promises an output, a ResourceSet stage names that output as its host", "".join(p))


def secrets_flow() -> str:
    W, H = 1200, 600
    p: list[str] = []
    # Providers.
    p.append(heading(40, 40, "Where a secret is read from"))
    providers = ["env", "file", "exec", "keyring", "systemd credential", "Secret Service", "macOS Keychain"]
    for i, name in enumerate(providers):
        y = 52 + i * 34
        p.append(chip(40, y, 200, name, CM, 28, 13, mono=True))
        p.append(flow(f"M240 {y + 14}L330 {y + 14}", CM, delay=i * 0.45, seconds=2.6, width=1.6))
    # Locked memory.
    p.append(rect(330, 52, 290, 238, WELL, CM, 18))
    p.append(f'<rect x="330" y="52" width="290" height="5" rx="2.5" fill="{CM}"/>')
    p.append(text(475, 84, "SecretValue", 18, 700, WHITE, mono=True))
    p.append(lines(475, 110, [
        "its own block of locked memory",
        "mlock: never swapped out",
        "a guard page on each side",
        "left out of core dumps",
        "wiped the moment it is dropped",
        "no Debug, Clone, Eq or Serialize",
        "read only by expose(|bytes| …)",
    ], 13, TEXT_SOFT))
    p.append(text(475, 276, "only crate meridian-secret holds the bytes", 11.5, 400, TEXT_SOFT, mono=True))
    # Everywhere else: a reference.
    p.append(heading(680, 40, "Everywhere else: a number, never the bytes"))
    p.append(rect(680, 52, 480, 110, TILE, WHITE))
    p.append(text(920, 84, "Value::Secret(3)", 17, 700, WHITE, mono=True))
    p.append(lines(920, 108, [
        "a plan, a log line, an event, the run record, an error, a panic:",
        "all made from values, so each shows (sensitive) and can show nothing else",
        "validate and compile fetch nothing: the table holds the name only",
    ], 12.5, TEXT_SOFT))
    p.append(flow("M620 110L680 110", WHITE, delay=0.3, seconds=2))
    # Into the artifact: a reference by name.
    p.append(rect(680, 180, 480, 110, TILE, IAC))
    p.append(f'<rect x="680" y="180" width="480" height="5" rx="2.5" fill="{IAC}"/>')
    p.append(text(920, 212, "compile writes a reference, by name", 16, 700, WHITE))
    p.append(lines(920, 238, [
        "ansible:    {{ meridian_secret_db_password }}   with no_log",
        "terraform:  ${var.meridian_secret_db_password}   sensitive = true",
        "a target with no private channel stops the stage and says so",
    ], 12.5, TEXT_SOFT, mono=True))
    p.append(flow("M620 236L680 236", IAC, delay=1.1, seconds=2))
    # The run: the private channel.
    ry = 330
    p.append(heading(40, ry - 12, "At plan and apply: the value, by a private channel and no other"))
    boxes = [
        (40, "the core", ["the run context carries", "the value over the", "plugin's private socket"], WHITE),
        (330, "the plugin SDK", ["takes it straight into", "protected memory and", "wipes the message"], LINE),
        (620, "a private file", ["mode 0600, in a 0700 directory,", "on a memory filesystem,", "overwritten and removed after"], IAC),
        (910, "the tool", ["reads it as its own", "variables file; what it", "prints is masked"], CM),
    ]
    for i, (x, title, body, colour) in enumerate(boxes):
        p.append(card(x, ry, 250, 108, title, body, colour, 16, 12.5))
        if i:
            p.append(flow(f"M{x - 40} {ry + 54}L{x} {ry + 54}", colour, delay=i * 0.5, seconds=2))
    # Never.
    ny = 470
    p.append(rect(40, ny, 1120, 70, WELL, RED, 14, 1.2))
    p.append(text(70, ny + 28, "Never", 16, 700, RED, "start"))
    p.append(text(70, ny + 52, "on a command line · in a tool's environment · in a compiled file · in the state directory · in a plan file Meridian keeps · in an event",
                  13, 400, TEXT_SOFT, "start"))
    p.append(caption(W, H, "The protection is structural: the bytes were never in what a log or a plan is made from, so there is nothing to forget to mask."))
    return doc(W, H, "How a secret moves: from a provider into locked memory, as a reference everywhere else, and to the tool by a private file at run time", "".join(p))


def run_safety() -> str:
    W, H = 1200, 640
    p: list[str] = []
    cx = 170
    p.append(card(40, 40, 260, 70, "meridian plan / apply", ["validate and compile take no lock"], WHITE, 16, 12.5, True))
    # Lock.
    p.append(flow(f"M{cx} 110L{cx} 150", WHITE, delay=0))
    p.append(card(40, 150, 260, 70, "take the state lock", ["one run at a time per state"], LINE, 16, 12.5))
    p.append(flow(f"M300 185L400 185", RED, delay=0.5, seconds=2.2))
    p.append(card(400, 150, 300, 70, "refused, told who holds it", ["nothing is run"], RED, 15, 12.5))
    # Policy.
    p.append(flow(f"M{cx} 220L{cx} 260", WHITE, delay=0.4))
    p.append(card(40, 260, 260, 70, "--policy: allowed?", ["every change is asked about first"], LINE, 16, 12.5))
    p.append(flow(f"M300 295L400 295", RED, delay=0.9, seconds=2.2))
    p.append(card(400, 260, 300, 70, "the run fails", ["nothing was applied"], RED, 15, 12.5))
    # Stages.
    p.append(flow(f"M{cx} 330L{cx} 370", WHITE, delay=0.8))
    p.append(card(40, 370, 260, 70, "run the stages in order", ["Infrastructure, then ResourceSets"], WHITE, 16, 12.5))
    # Record and state on the right.
    p.append(flow(f"M300 405L740 405", CON, delay=0.2, seconds=2.6))
    p.append(text(520, 396, "every change of state is an event", 12.5, 400, TEXT_SOFT))
    p.append(card(740, 340, 200, 128, "the record", ["runs/<id>/run.json", "runs/<id>/events.jsonl", "names things, holds", "no value and no secret"], CON, 16, 12.5, True))
    p.append(card(960, 340, 200, 128, "the run-state", ["state.json", "which stage applied", "which artifact, by hash;", "outputs are read back"], CON, 16, 12.5, True))
    p.append(flow(f"M840 468L840 520", CON, delay=1, seconds=2))
    p.append(flow(f"M1060 468L1060 520", CON, delay=1.4, seconds=2))
    p.append(rect(740, 520, 420, 56, WELL, CON, 28, 1.2))
    p.append(text(950, 543, "backend", 15, 700, WHITE))
    p.append(text(950, 562, "the state directory, or a store a team shares", 12.5, 400, TEXT_SOFT))
    # Done or failed.
    p.append(flow(f"M{cx} 440L{cx} 480", WHITE, delay=1.2))
    p.append(card(40, 480, 260, 70, "all stages done?", ["yes: release the lock"], GREEN, 16, 12.5))
    p.append(flow(f"M300 515L400 515", CM, delay=1.6, seconds=2.2))
    p.append(card(400, 480, 300, 112, "a stage failed: --on-failure", [
        "stop (default): release the lock;",
        "apply --resume continues there",
        "rollback: put back every stage",
        "this run changed, last first",
    ], CM, 15, 12.5))
    # Drift, off to the side of the record.
    p.append(rect(400, 40, 760, 90, TILE, LINE, 14, 1.2, "5 5"))
    p.append(text(430, 68, "Afterwards", 15, 700, WHITE, "start"))
    p.append(lines(430, 92, [
        "meridian drift asks each tool whether what was applied is still what is there. meridian rollback puts a finished run back.",
        "Each tool owns its own state: Meridian's record says what Meridian did, never the truth about your infrastructure.",
    ], 12.5, TEXT_SOFT, "start"))
    p.append(caption(W, H, "A run can be repeated, shared, picked up where it stopped, or put back, because every step of it was recorded before the next began."))
    return doc(W, H, "Running safely: the lock, the policy check, the record of every event, the run-state, and what happens when a stage fails", "".join(p))


def plugin_categories() -> str:
    W, H = 1200, 470
    p: list[str] = []
    p.append(heading(40, 40, "Three kinds of document"))
    p.append(card(40, 52, 320, 100, "kind: Infrastructure", ["what should exist?", "creates things through an API,", "keeps track of them, promises outputs"], IAC, 18, 13.5))
    p.append(card(40, 182, 320, 100, "kind: ResourceSet", ["how should this machine be set up?", "logs in to a machine that exists,", "converges packages, files, services"], CM, 18, 13.5))
    p.append(card(40, 312, 320, 100, "kind: Workflow", ["in what order, on what, with what gates?", "steps over the other documents;", "an engine's job runs each step"], PIPE, 18, 13.5))
    p.append(heading(560, 40, "Five categories a plugin can declare"))
    cats = [
        ("Infrastructure as code", IAC, "terraform, opentofu, cloudformation, bicep, arm, pulumi, packer", 52),
        ("Containers", CON, "docker takes either kind; podman", 122),
        ("Orchestration", ORC, "kubernetes, helm", 192),
        ("Configuration management", CM, "ansible, puppet, openvox, chef, salt, dsc, cloud-init", 262),
        ("Pipelines", PIPE, "github-actions, azure-pipelines, gitlab-ci, buildkite, argo, octopus", 332),
    ]
    for label, colour, members, y in cats:
        p.append(rect(560, y, 600, 56, TILE, colour))
        p.append(f'<rect x="560" y="{y}" width="5" height="56" rx="2.5" fill="{colour}"/>')
        p.append(text(590, y + 24, label, 16, 700, colour, "start"))
        p.append(text(590, y + 45, members, 13, 400, TEXT_SOFT, "start", mono=True))
    # Infrastructure reaches three categories; ResourceSet one, plus docker; Workflow the pipelines.
    p.append(flow("M360 102C460 102 460 80 560 80", IAC, delay=0, seconds=2.4))
    p.append(flow("M360 102C460 102 460 150 560 150", IAC, delay=0.6, seconds=2.4))
    p.append(flow("M360 102C460 102 460 220 560 220", IAC, delay=1.2, seconds=2.4))
    p.append(flow("M360 232C460 232 460 290 560 290", CM, delay=0.3, seconds=2.4))
    p.append(flow("M360 232C460 232 460 150 560 150", CM, delay=1.5, seconds=2.4))
    p.append(flow("M360 362C460 362 460 360 560 360", PIPE, delay=0.9, seconds=2.4))
    p.append(caption(W, H, "A document aimed at a category that does not take its kind is refused at compile, with the reason. Each plugin declares which it is."))
    return doc(W, H, "Which kind of document each plugin category takes: Infrastructure for infrastructure as code, containers and orchestration; ResourceSet for configuration management; Workflow for pipelines; docker takes both of the first two", "".join(p))


def in_a_pipeline() -> str:
    """What a pipeline engine keeps, what Meridian owns, and the three things that join them."""
    W, H = 1200, 640
    p: list[str] = []
    # The engine's band.
    ey, eh = 48, 118
    p.append(gutter(ey, eh, "The engine keeps", PIPE))
    p.append(rect(90, ey - 12, 1070, eh + 24, WELL, PIPE, 18, 1.2, "6 6"))
    engine = [
        ("trigger", ["push, tag, schedule,", "pull request"]),
        ("runner + identity", ["a machine, and the OIDC", "token only it can mint"]),
        ("secret release", ["the store, released to", "a job after its rules pass"]),
        ("approval", ["reviewers, wait timers,", "the audit trail"]),
        ("logs + artifacts", ["every job's output,", "kept and linked"]),
    ]
    ex, ew, egap = 110, 198, 15
    ecx = []
    for i, (title, body) in enumerate(engine):
        x = ex + i * (ew + egap)
        p.append(card(x, ey, ew, eh - 10, title, body, PIPE, 16, 12.5))
        ecx.append(x + ew / 2)
        if i:
            p.append(flow(f"M{x - egap:g} {ey + 54}L{x:g} {ey + 54}", PIPE, delay=i * 0.4, seconds=1.4, width=1.5))
    # Meridian's band.
    my, mh = 330, 150
    p.append(gutter(my, mh, "Meridian owns", WHITE))
    p.append(rect(90, my - 12, 1070, mh + 24, TILE, LINE, 18))
    p.append(card(110, my, 220, mh - 10, "Workflow", ["steps, order from needs,", "fan-out, gates, retry,", "finally, with: values"], WHITE, 17, 12.5))
    p.append(card(350, my, 290, mh - 10, "orchestrator", ["lock, run record, events,", "--resume, rollback, drift,", "policy, a shared backend", "across every job"], WHITE, 17, 12.5))
    p.append(card(660, my, 300, mh - 10, "target plugins", ["terraform, ansible, kubernetes, ...", "each compiles its tool's own files", "and drives the tool"], LINE, 17, 12.5))
    p.append(card(980, my, 180, mh - 10, "secret delivery", ["each tool's own", "channel; a value", "is never printed"], ORC, 17, 12.5))
    p.append(flow(f"M330 {my + 70}L350 {my + 70}", WHITE, delay=0.2, seconds=1.2, width=1.5))
    p.append(flow(f"M640 {my + 70}L660 {my + 70}", WHITE, delay=0.6, seconds=1.2, width=1.5))
    p.append(flow(f"M960 {my + 70}L980 {my + 70}", WHITE, delay=1.0, seconds=1.2, width=1.5))
    # The joins run in lanes between the bands, so each can be read on its own.
    top, bottom = ey + eh - 10, my
    runner, secret, approval, logs, trigger = ecx[1], ecx[2], ecx[3], ecx[4], ecx[0]
    p.append(flow(f"M{runner:g} {top}L{runner:g} {bottom}", WHITE, delay=0.3))
    p.append(text(runner - 12, 236, "runs meridian apply", 13, 600, WHITE, "end", mono=True))
    p.append(text(runner - 12, 254, "one job, or one job per step", 12, 400, TEXT_SOFT, "end"))
    p.append(flow(f"M{secret:g} {top}L{secret:g} 222L1120 222L1120 {bottom}", ORC, delay=0.9, seconds=3.4))
    p.append(text(1108, 214, "env and OIDC: a secret provider", 12.5, 400, TEXT_SOFT, "end"))
    p.append(flow(f"M{approval:g} {top}L{approval:g} 262L560 262L560 {bottom}", PIPE, delay=1.4, seconds=3.4))
    p.append(text(574, 254, "gate satisfied by the engine's approval", 12.5, 400, TEXT_SOFT, "start"))
    p.append(flow(f"M600 {bottom}L600 298L{logs:g} 298L{logs:g} {top}", CON, delay=0.6, seconds=3.4))
    p.append(text(614, 316, "--events json: annotations, summaries, the record", 12.5, 400, TEXT_SOFT, "start"))
    p.append(f'<path d="M{trigger + 6:g} {bottom}L{trigger + 6:g} {top}" fill="none" stroke="{PIPE}" stroke-width="1.5" stroke-dasharray="5 6" marker-end="url(#g-arrow)" opacity=".8"/>')
    p.append(text(trigger + 18, 290, "an emitter plugin writes the job", 12, 400, TEXT_SOFT, "start"))
    p.append(text(trigger + 18, 306, "skeleton from the Workflow;", 12, 400, TEXT_SOFT, "start"))
    p.append(text(trigger + 18, 322, "every job still runs meridian", 12, 400, TEXT_SOFT, "start"))
    # Below: what this is not.
    p.append(rect(90, 510, 1070, 74, TILE, LINE, 14, 1.2, "5 5"))
    p.append(text(120, 536, "What stays where it is", 15, 700, WHITE, "start"))
    p.append(lines(120, 558, [
        "Meridian never becomes the engine: no triggers, runners, reviewer lists or log hosting. The engine never does the work itself: no terraform apply in a job, only meridian.",
    ], 12.5, TEXT_SOFT, "start"))
    p.append(caption(W, H, "The engine is a thin shell: it starts the job, holds the secrets and the approval, and keeps the logs. Everything the job does is Meridian's."))
    return doc(W, H, "Meridian inside a pipeline engine: the engine keeps the trigger, the runner and its identity, the secret release, the approval and the logs; Meridian owns the workflow, the orchestrator, the target plugins and secret delivery", "".join(p))


def saved_plan() -> str:
    """A saved plan: what plan --out writes, what the reviewer sees, what apply --plan checks and hands on."""
    W, H = 1200, 560
    p: list[str] = []
    # The plan, on the left.
    p.append(heading(40, 40, "meridian plan --out plan.mplan", WHITE))
    p.append(card(40, 56, 300, 150, "plan", ["compiles every document,", "asks each tool what would change,", "asks the policy, and writes the file"], LINE, 17, 12.5))
    p.append(flow("M340 131L400 131", WHITE, delay=0.2, seconds=1.6))
    # The file, in the middle.
    p.append(rect(400, 56, 380, 290, WELL, PIPE, 18, 1.4))
    p.append(f'<rect x="400" y="56" width="380" height="5" rx="2.5" fill="{PIPE}"/>')
    p.append(text(590, 88, "plan.mplan", 18, 700, WHITE, mono=True))
    p.append(lines(422, 118, [
        "files, data, set, selector, run",
        "stages:",
        "  network  (terraform)",
        "    artifactHash: sha256:9f2c…",
        "    changes: create web_vm, …",
        "    appliedBefore: sha256:40b1…",
        "    saved: <the tool's own plan>",
        "    policy: allowed",
        "  web  (ansible)   status: deferred",
    ], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(chip(422, 300, 336, "names, hashes and summaries: never a value", GREEN, 28, 12))
    # The reviewer, above right.
    p.append(card(840, 56, 320, 110, "the pull request", ["one comment, updated in place:", "the report of the plan", "and the policy's decision"], PIPE, 17, 12.5))
    p.append(flow("M780 111L840 111", PIPE, delay=0.8, seconds=1.6))
    # The apply, below right.
    p.append(heading(840, 212, "meridian apply --plan plan.mplan", WHITE))
    p.append(card(840, 228, 320, 118, "apply", ["the same stages, none applied since;", "each document compiles to the", "planned hash, or plan-differs;", "each tool is handed its saved plan"], LINE, 17, 12.5))
    p.append(flow("M780 287L840 287", WHITE, delay=1.4, seconds=1.6))
    # The checks, as a row along the bottom.
    cy = 410
    p.append(heading(40, cy - 12, "What is refused, and when"))
    checks = [
        ("before anything runs", ["a stage the plan does not have,", "or one the run does not;", "a stage applied since the plan"], RED),
        ("at each stage's turn", ["the document no longer compiles", "to the planned artifact:", "plan-differs, and the run stops"], RED),
        ("by the tool itself", ["Terraform and OpenTofu refuse a", "stale plan file; Pulumi a changed", "plan; CloudFormation an obsolete", "change set"], IAC),
        ("never checked", ["a stage the plan deferred, which", "waited on an output: applied as", "any apply would, with a warning"], TEXT_SOFT),
    ]
    for i, (title, body, colour) in enumerate(checks):
        x = 40 + i * 285
        p.append(card(x, cy, 265, 118, title, body, colour, 15, 12.5))
    p.append(caption(W, H, "What a reviewer approved is what is applied: a plan file ties the two, and anything that moved in between is a refusal, not a surprise."))
    return doc(W, H, "A saved plan: meridian plan --out writes a file of artifact hashes, changes and each tool's own saved plan; the pull request shows it as one comment; meridian apply --plan refuses a stale plan and hands each tool the plan it saved", "".join(p))


def modules_flow() -> str:
    W, H = 1200, 440
    p: list[str] = []
    p.append(card(40, 60, 250, 130, "meridian.yaml", ["requires:", "  acme/nginx: ^1.2", "    git: …/modules.git", "  acme/hardening: 2.x", "    oci: ghcr.io/acme/…"], WHITE, 17, 12.5, True))
    srcs = [("path", "a directory on disk", IAC, 60), ("git", "a repository, by tag or branch", CM, 110), ("oci", "a container registry", CON, 160)]
    for i, (name, note, colour, y) in enumerate(srcs):
        p.append(chip(350, y, 70, name, colour, 28, 13, True))
        p.append(text(430, y + 19, note, 12.5, 400, TEXT_SOFT, "start"))
        p.append(flow(f"M290 125C320 125 320 {y + 14} 350 {y + 14}", colour, delay=i * 0.5, seconds=2.4))
    p.append(card(640, 60, 250, 130, "meridian module install", ["fetches each module once,", "picks one version per project", "from its SemVer requirement,", "runs nothing from it"], LINE, 16, 12.5))
    p.append(flow("M610 125L640 125", WHITE, delay=0.4, seconds=1.6))
    p.append(card(930, 60, 230, 130, "meridian.lock", ["each module pinned by a", "hash of its content;", "commit it, never edit it"], GREEN, 17, 12.5, True))
    p.append(flow("M890 125L930 125", GREEN, delay=0.8, seconds=1.6))
    # Every run.
    ry = 250
    p.append(heading(250, ry - 12, "Every compile, plan and apply"))
    p.append(card(40, ry, 340, 96, "verify, never fetch", ["the hash of each module is checked", "against the lockfile before it is read;", "a change since the pin is refused"], WHITE, 16, 12.5))
    p.append(flow(f"M1045 190L1045 218L210 218L210 {ry}", GREEN, delay=0.2, seconds=3.2))
    p.append(card(420, ry, 340, 96, "a module's file in a document", ["@acme/nginx:documents/proxy.yaml", "names a file inside the module", "for any command to use"], IAC, 16, 12.5, True))
    p.append(flow(f"M380 {ry + 48}L420 {ry + 48}", WHITE, delay=1.1, seconds=1.6))
    p.append(card(800, ry, 360, 96, "--module-signatures", ["optional: a fetched module must be", "signed by a key you trust;", "meridian module sign makes one"], ORC, 16, 12.5))
    p.append(flow(f"M760 {ry + 48}L800 {ry + 48}", ORC, delay=1.6, seconds=1.6))
    p.append(caption(W, H, "There is no central index. A requirement says where its module comes from: your git server, or your container registry."))
    return doc(W, H, "Modules: a manifest names what a project needs, install fetches and pins it in the lockfile, and every run verifies the pin without fetching", "".join(p))


def test_flow() -> str:
    W, H = 1200, 470
    p: list[str] = []
    p.append(card(40, 60, 300, 150, "tests/site.test.yaml", ["kind: Test", "documents: [../site.yaml]", "cases:", "  - name: prod", "    set: {env: prod}", "    expect: …  assert: …"], WHITE, 17, 12.5, True))
    p.append(heading(400, 48, "For each case, in a scratch directory of its own"))
    steps = [("data", "the case's files and set", IAC), ("compile", "every document, by its plugin", CM), ("plan", "only when run: plan; from nothing applied", ORC)]
    for i, (name, note, colour) in enumerate(steps):
        x = 400 + i * 270
        p.append(card(x, 60, 240, 76, name, [note], colour, 16, 12.5, i == 0))
        if i > 0:
            p.append(flow(f"M{x - 30} 98L{x} 98", colour, delay=i * 0.4, seconds=1.6))
    p.append(flow("M340 135C370 135 370 98 400 98", WHITE, delay=0.0, seconds=1.6))
    # What comes out.
    oy = 200
    p.append(card(400, oy, 780, 70, "stages.<document>", ["outcome · files · warnings · refusal {rule, resource, message} · changes · condition"], LINE, 16, 12.5))
    p.append(flow(f"M1060 136L1060 {oy}", ORC, delay=1.2, seconds=1.6))
    p.append(flow(f"M790 136L790 {oy}", CM, delay=0.8, seconds=1.6))
    # The checks.
    cy = 320
    p.append(heading(40, cy - 12, "Checks, all of them reported"))
    p.append(card(40, cy, 360, 96, "expect", ["outcome: refused   rule: unsupported-action", "files: {playbook.yml: {contains: …}}", "changes: [{action: create}]  changeCount: 2"], GREEN, 16, 12.5, True))
    p.append(card(440, cy, 360, 96, "assert", ["stages.web.outcome == 'compiled'", "contains(stages.web.files['playbook.yml'], 'x')", "the one expression language, no script"], GREEN, 16, 12.5, True))
    p.append(card(840, cy, 320, 96, "report", ["ok / FAIL per case, with each failure;", "--output json: one object;", "--junit FILE: for the pipeline engine"], WHITE, 16, 12.5))
    p.append(flow(f"M790 270L790 {cy}", GREEN, delay=1.6, seconds=1.6))
    p.append(flow(f"M400 {cy + 48}L440 {cy + 48}", GREEN, delay=2.0, seconds=1.6))
    p.append(flow(f"M800 {cy + 48}L840 {cy + 48}", WHITE, delay=2.4, seconds=1.6))
    p.append(caption(W, H, "Nothing is applied and no record is kept: a test compiles and plans, then checks. A module carries its tests under tests/."))
    return doc(W, H, "meridian test: a Test document names documents and cases; each case compiles or plans them with its data in a scratch directory and checks the outcome, the files, the refusal and the changes", "".join(p))


def schema_bundles() -> str:
    """Schema bundles: published once from the tools, then synced by every project without them."""
    W, H = 1200, 560
    p: list[str] = []
    # Publishing, along the top.
    p.append(heading(40, 40, "Published once, where the tools are"))
    top = [
        ("catalogue.yaml", ["each upstream at one", "exact version, moved", "by a reviewed PR"], WHITE, False),
        ("build", ["no secrets:", "sync --all-types", "and pack each bundle"], IAC, False),
        ("publish", ["a reviewer approves;", "sign, push, update", "the index, attest"], PIPE, False),
    ]
    for i, (title, body, colour, mono) in enumerate(top):
        x = 40 + i * 250
        p.append(card(x, 56, 220, 118, title, body, colour, 16, 12.5, mono))
        if i > 0:
            p.append(flow(f"M{x - 30} 115L{x} 115", colour, delay=i * 0.5, seconds=1.6))
    # The registry, top right.
    p.append(rect(800, 56, 360, 118, WELL, CON, 18, 1.4))
    p.append(f'<rect x="800" y="56" width="360" height="5" rx="2.5" fill="{CON}"/>')
    p.append(text(980, 86, "an OCI registry, or a directory", 16, 700, WHITE))
    p.append(lines(822, 112, [
        "terraform/hashicorp-aws:5.60.0",
        "kubernetes/core:1.31.0",
        "index:latest   every bundle, its types",
    ], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(flow("M770 115L800 115", CON, delay=1.5, seconds=1.6))
    # Using, along the bottom.
    by = 280
    p.append(heading(40, by - 16, "Used everywhere else, with no tool"))
    p.append(card(40, by, 250, 130, "meridian.yaml", ["spec:", "  schemas:", "    registry: ghcr.io/…", "    signatures: require"], WHITE, 16, 12.5, True))
    p.append(card(330, by, 300, 130, "meridian schema sync", ["the types the documents use", "→ the newest bundle that", "describes each, from the index", "→ its signature checked"], LINE, 16, 12.5))
    p.append(flow(f"M290 {by + 65}L330 {by + 65}", WHITE, delay=0.2, seconds=1.6))
    p.append(flow(f"M980 174L980 222L480 222L480 {by}", CON, delay=0.6, seconds=3.0))
    p.append(card(670, by, 230, 130, "the cache", ["types used: in full", "the rest: by name", "the same bytes on", "every machine"], ORC, 16, 12.5))
    p.append(flow(f"M630 {by + 65}L670 {by + 65}", ORC, delay=1.0, seconds=1.6))
    p.append(card(940, by, 220, 130, "meridian.lock", ["each bundle by", "reference and digest;", "sync --locked takes", "exactly those"], GREEN, 16, 12.5))
    p.append(flow(f"M900 {by + 65}L940 {by + 65}", GREEN, delay=1.4, seconds=1.6))
    # The fallback, and the editor.
    fy = 448
    p.append(chip(330, fy, 300, "no bundle describes a type: ask the tool", CM, 30, 12.5))
    p.append(flow(f"M480 {by + 130}L480 {fy}", CM, delay=1.8, seconds=1.6))
    p.append(chip(670, fy, 490, "the editor, lint and the JSON Schema read the cache as before", LINE, 30, 12.5))
    p.append(flow(f"M785 {by + 130}L785 {fy}", ORC, delay=2.2, seconds=1.6))
    p.append(caption(W, H, "A bundle is data, signed and pinned by digest: it can make completion wrong, never a deployment."))
    return doc(W, H, "Schema bundles: a catalogue is built into signed bundles and pushed with an index to an OCI registry; a project's schema sync picks the newest bundle for each type its documents use, checks the signature, writes the cache and pins each bundle in meridian.lock, and asks the tool only for what no bundle describes", "".join(p))


def adopt_flow() -> str:
    """adopt: one field on a resource, and what each kind of target makes of it."""
    W, H = 1200, 470
    p: list[str] = []
    p.append(card(40, 60, 300, 150, "a resource", None, WHITE, 17))
    p.append(lines(80, 112, ["- id: logs", "  type: aws_s3_bucket", "  adopt: example-logs", "  params:", "    bucket: example-logs"], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(heading(400, 48, "What the target says it does with adopt"))
    kinds = [
        ("import", ["compiled into the tool's own import:", "an import block, Pulumi's import", "option. The first plan says adopt,", "the apply puts it in the state"], IAC, "Terraform · OpenTofu · Pulumi"),
        ("converge", ["accepted with a warning, and", "changes nothing: the tool keeps", "no state and makes the thing what", "the document says either way"], CM, "Ansible · Puppet · Chef · Salt · DSC · Kubernetes · Bicep · ARM"),
        ("none", ["refused by the core before the", "plugin is asked, so no tool", "creates a second thing beside", "the one that exists"], RED, "every other target, and any plugin built before adopt"),
    ]
    for i, (name, body, colour, who) in enumerate(kinds):
        x = 400 + i * 265
        p.append(card(x, 60, 245, 150, name, body, colour, 16, 12.5))
    p.append(flow("M340 135L400 135", WHITE, delay=0.2, seconds=1.6))
    # Who is which, below.
    y = 250
    p.append(heading(40, y - 12, "Which targets"))
    for i, (name, _, colour, who) in enumerate(kinds):
        p.append(chip(40, y + i * 40, 110, name, colour, 28, 13, True))
        p.append(text(165, y + i * 40 + 19, who, 13, 400, TEXT_SOFT, "start"))
    p.append(card(760, 250, 400, 108, "meridian plan", None, GREEN, 16))
    p.append(lines(790, 302, ["= adopt logs: aws_s3_bucket", "~ update app: aws_subnet, adopted"], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(text(960, 346, "an import that also changes the thing is reported as the change", 11.5, 400, TEXT_SOFT))
    p.append(caption(W, H, "One field moves between tools: a stateful tool imports the thing, a converging tool already takes it as it finds it."))
    return doc(W, H, "adopt: a resource names the tool's identifier of a thing that exists; a stateful target compiles it into the tool's import and the plan reports adopt, a converging target accepts it and changes nothing, and any other target is refused before its plugin is asked", "".join(p))


def retarget_flow() -> str:
    """meridian retarget: the old tool reports what it manages, the new file adopts it."""
    W, H = 1200, 480
    p: list[str] = []
    p.append(heading(40, 48, "One document, from one tool to another"))
    p.append(card(40, 60, 330, 170, "site.yaml", None, IAC, 17))
    p.append(lines(70, 112, ["spec:", "  target: terraform", "  resources:", "    - id: logs", "      type: aws_s3_bucket"], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(card(435, 60, 330, 170, "the old tool's state", None, WHITE, 17))
    p.append(lines(465, 112, ["terraform show -json", "", "logs  ->  example-logs", "web   ->  i-0abc123"], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(card(830, 60, 330, 170, "site.opentofu.yaml", None, GREEN, 17))
    p.append(lines(860, 112, ["spec:", "  target: opentofu", "  resources:", "    - id: logs", "      adopt: \"example-logs\""], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(flow("M370 145L435 145", WHITE, delay=0.2, seconds=1.6))
    p.append(flow("M765 145L830 145", WHITE, delay=0.9, seconds=1.6))
    p.append(text(602, 255, "meridian retarget site.yaml --to opentofu", 13, 600, WHITE, mono=True))
    p.append(heading(40, 300, "What the new target decides"))
    kinds = [
        ("import", "an adopt line per resource the old tool manages", IAC),
        ("converge", "only the target: the tool takes what it finds", CM),
        ("none", "only the target, and a warning: it creates again", RED),
    ]
    for i, (name, body, colour) in enumerate(kinds):
        p.append(chip(40, 312 + i * 38, 110, name, colour, 28, 13, True))
        p.append(text(165, 312 + i * 38 + 19, body, 13, 400, TEXT_SOFT, "start"))
    p.append(card(760, 300, 400, 96, "meridian plan site.opentofu.yaml", None, GREEN, 15))
    p.append(lines(790, 350, ["= adopt logs: aws_s3_bucket", "= adopt web: aws_instance"], 12.5, TEXT_SOFT, "start", mono=True))
    p.append(caption(W, H, "The old file is left as it was, and no tool changes anything until you plan and apply the new one."))
    return doc(W, H, "retarget: the old tool reports the identifier of each resource it manages, meridian retarget writes a new file with the new target and an adopt line per resource, and the new tool's first plan adopts each one instead of creating it", "".join(p))


GRAPHIC_FILES = {
    "meridian-architecture.svg": architecture,
    "meridian-compile-pipeline.svg": compile_pipeline,
    "meridian-two-stage-workflow.svg": two_stage,
    "meridian-secrets-flow.svg": secrets_flow,
    "meridian-run-safety.svg": run_safety,
    "meridian-plugin-categories.svg": plugin_categories,
    "meridian-in-a-pipeline.svg": in_a_pipeline,
    "meridian-modules-flow.svg": modules_flow,
    "meridian-saved-plan.svg": saved_plan,
    "meridian-test-flow.svg": test_flow,
    "meridian-schema-bundles.svg": schema_bundles,
    "meridian-adopt.svg": adopt_flow,
    "meridian-retarget.svg": retarget_flow,
}

# ---------------------------------------------------------------------------
# The animated marks, derived from the still branding files
# ---------------------------------------------------------------------------

MARK_STYLE = """
.m-light{stroke-dasharray:14 86;animation:m-travel 2.6s linear infinite}
.m-glow{stroke-dasharray:14 86;opacity:.5;animation:m-travel 2.6s linear infinite}
.m-point{animation:m-breathe 2.6s ease-in-out infinite;transform-origin:center;transform-box:fill-box}
@keyframes m-travel{to{stroke-dashoffset:-100}}
@keyframes m-breathe{0%,100%{opacity:.25;transform:scale(1)}50%{opacity:.6;transform:scale(1.35)}}
@media (prefers-reduced-motion:reduce){.m-light,.m-glow{display:none}.m-point{animation:none}}
""".strip()

MERIDIAN_PATH = re.compile(r'<path d="([^"]+)" stroke="(#[0-9A-Fa-f]{6})"( stroke-width="[^"]*")?/>')


def with_light(still: str, label: str, point_colour: str, light: str, width: float) -> str:
    """Add the travelling light to the meridians of a still logo or banner.

    Each coloured meridian gets two overlays with `pathLength="100"`: a soft
    one in its own colour and a sharp one in the light's colour, offset so the
    five lights leave the point one after another.
    """
    found = MERIDIAN_PATH.findall(still)
    if len(found) != 5:
        raise SystemExit(f"{label}: expected the five meridians, found {len(found)}")
    overlays = []
    for i, (d, colour, _) in enumerate(found):
        delay = f"animation-delay:{-i * 0.52:g}s"
        overlays.append(
            f'<path d="{d}" stroke="{colour}" stroke-width="{width * 2:g}" pathLength="100" class="m-glow" style="{delay}"/>'
            f'<path d="{d}" stroke="{light}" stroke-width="{width:g}" pathLength="100" class="m-light" style="{delay}"/>'
        )
    group = (f'<g fill="none" stroke-linecap="round">{"".join(overlays)}</g>')
    point = re.search(r'<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)"[^/]*fill="' + re.escape(point_colour) + '"/>', still)
    if not point:
        raise SystemExit(f"{label}: the point was not found")
    cx, cy, r = point.groups()
    halo = f'<circle cx="{cx}" cy="{cy}" r="{float(r) * 1.9:g}" fill="{point_colour}" class="m-point"/>'
    out = still.replace("<title>", f"<style>{MARK_STYLE}</style><title>", 1)
    # The overlays go right after the still meridians, under the point.
    out = out.replace(point.group(0), group + halo + point.group(0), 1)
    out = re.sub(r'aria-label="[^"]*"', f'aria-label="{esc(label)}"', out, count=1)
    out = re.sub(r"<title>.*?</title>", f"<title>{esc(label)}</title>", out, count=1, flags=re.DOTALL)
    return out


def strip_metadata(still: str) -> str:
    return re.sub(r"<metadata>.*?</metadata>", "", still, flags=re.DOTALL).replace(' xmlns:c2pa="http://c2pa.org/manifest"', "")


def animated_logo() -> str:
    still = strip_metadata((BRANDING / "logos" / "meridian-core.svg").read_text(encoding="utf-8"))
    # The light rides the meridians; the glyph's own id must not clash with the still file when both are inlined.
    still = still.replace("mg-core", "mg-core-animated")
    return with_light(still, "Meridian Core", "#fff", "#FFFFFF", 2.2) + "\n"


def animated_banner(scheme: str) -> str:
    still = (BRANDING / "banner" / f"meridian-banner-{scheme}.svg").read_text(encoding="utf-8")
    point_colour = "#FFFFFF" if scheme == "dark" else "#0F1B3D"
    light = "#FFFFFF"
    still = still.replace("mb-bg", f"mb-bg-{scheme}-animated").replace("mb-clip", f"mb-clip-{scheme}-animated")
    return with_light(still, "Meridian: one DSL for everything", point_colour, light, 5)


DERIVED_FILES = {
    BRANDING / "logos" / "meridian-core-animated.svg": animated_logo,
    BRANDING / "banner" / "meridian-banner-dark-animated.svg": lambda: animated_banner("dark"),
    BRANDING / "banner" / "meridian-banner-light-animated.svg": lambda: animated_banner("light"),
}


def every_file() -> dict[Path, str]:
    out = {GRAPHICS / name: draw() for name, draw in GRAPHIC_FILES.items()}
    out.update({path: draw() for path, draw in DERIVED_FILES.items()})
    return out


def main(argv: list[str]) -> int:
    check = "--check" in argv
    stale = []
    for path, content in every_file().items():
        rel = path.relative_to(ROOT).as_posix()
        if check:
            if not path.exists() or path.read_text(encoding="utf-8") != content:
                stale.append(rel)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        print(f"wrote {rel}")
    if stale:
        print("out of date; run python3 tools/wiki/graphics.py:", file=sys.stderr)
        for rel in stale:
            print(f"  {rel}", file=sys.stderr)
        return 1
    if check:
        print(f"{len(GRAPHIC_FILES) + len(DERIVED_FILES)} graphics are up to date")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
