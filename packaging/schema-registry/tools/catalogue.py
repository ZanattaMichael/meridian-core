#!/usr/bin/env python3
"""The helper the schema registry's workflow runs.

  catalogue.py matrix                      the build matrix, as JSON
  catalogue.py seed TARGET NAME DIR        writes the seed document for one bundle into DIR
  catalogue.py show TARGET NAME FIELD      prints one field of one bundle (version, prefix)
  catalogue.py bump [--write]              looks up each upstream's newest version
  catalogue.py diff OLD.tar.gz NEW.tar.gz  what changed between two bundles, as Markdown

It reads catalogue.yaml beside its own directory and needs PyYAML.
"""

import datetime
import io
import json
import os
import re
import sys
import tarfile
import urllib.request

import yaml

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CATALOGUE = os.path.join(ROOT, "catalogue.yaml")


def load():
    with open(CATALOGUE, encoding="utf-8") as f:
        return yaml.safe_load(f)


def find(catalogue, target, name):
    for entry in catalogue["bundles"]:
        if entry["target"] == target and entry["name"] == name:
            return entry
    sys.exit(f"catalogue.yaml has no bundle {target}/{name}")


def matrix():
    entries = [
        {"target": e["target"], "name": e["name"], "version": str(e["version"])}
        for e in load()["bundles"]
    ]
    only = os.environ.get("ONLY", "").strip()
    if only:
        wanted = {w.strip() for w in only.split(",") if w.strip()}
        entries = [e for e in entries if f"{e['target']}/{e['name']}" in wanted]
    print(json.dumps({"include": entries}))


def seed(target, name, out_dir):
    entry = find(load(), target, name)
    with open(os.path.join(ROOT, entry["seed"]), encoding="utf-8") as f:
        text = f.read()
    upstream = entry.get("upstream", {})
    provider = upstream.get("provider", "")
    replacements = {
        "@VERSION@": str(entry["version"]),
        "@NAME@": entry["name"],
        "@SOURCE@": provider,
        "@LOCAL@": provider.split("/")[-1],
    }
    for key, value in replacements.items():
        text = text.replace(key, value)
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "seed.yaml"), "w", encoding="utf-8") as f:
        f.write(text)


def show(target, name, field):
    value = find(load(), target, name).get(field, "")
    if isinstance(value, list):
        print(" ".join(value))
    elif isinstance(value, dict):
        print(json.dumps(value))
    else:
        print(value)


def fetch_json(url):
    request = urllib.request.Request(url, headers={"User-Agent": "meridian-schemas-bump"})
    token = os.environ.get("GITHUB_TOKEN")
    if token and url.startswith("https://api.github.com/"):
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def fetch_text(url):
    request = urllib.request.Request(url, headers={"User-Agent": "meridian-schemas-bump"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read().decode().strip()


def plain(version):
    """A release version with no pre-release part, as a tuple to compare."""
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", version)
    return tuple(int(p) for p in match.groups()) if match else None


def newest(versions):
    found = [(plain(v), v.lstrip("v")) for v in versions if plain(v)]
    return max(found)[1] if found else None


def newest_version(entry):
    upstream = entry.get("upstream", {})
    kind = upstream.get("kind")
    if kind == "terraform-registry":
        data = fetch_json(
            f"https://registry.terraform.io/v1/providers/{upstream['provider']}/versions"
        )
        return newest(v["version"] for v in data["versions"])
    if kind == "github-release":
        data = fetch_json(
            f"https://api.github.com/repos/{upstream['repository']}/releases?per_page=30"
        )
        return newest(r["tag_name"] for r in data if not r["prerelease"] and not r["draft"])
    if kind == "pypi":
        data = fetch_json(f"https://pypi.org/pypi/{upstream['package']}/json")
        return newest(data["releases"].keys())
    if kind == "galaxy":
        namespace, name = upstream["collection"].split(".")
        data = fetch_json(
            "https://galaxy.ansible.com/api/v3/plugin/ansible/content/published/collections/"
            f"index/{namespace}/{name}/"
        )
        return data["highest_version"]["version"]
    if kind == "kind-node":
        stable = fetch_text("https://dl.k8s.io/release/stable.txt")
        return stable.lstrip("v")
    if kind == "date":
        # A vendor that publishes no version is read as it stands each week.
        return datetime.date.today().isoformat()
    return None


def bump(write):
    with open(CATALOGUE, encoding="utf-8") as f:
        text = f.read()
    catalogue = yaml.safe_load(text)
    lines = []
    for entry in catalogue["bundles"]:
        label = f"{entry['target']}/{entry['name']}"
        current = str(entry["version"])
        try:
            found = newest_version(entry)
        except Exception as e:  # one upstream that cannot be read stops nothing
            lines.append(f"- `{label}`: could not look up a newer version ({e})")
            continue
        if not found or found == current:
            continue
        lines.append(f"- `{label}`: {current} → {found}")
        # Changed in place, line by line, so comments and layout stay.
        pattern = re.compile(
            r"(- target: " + re.escape(entry["target"]) + r"\n\s+name: "
            + re.escape(entry["name"]) + r"\n\s+version: )\"?" + re.escape(current) + r"\"?"
        )
        text, n = pattern.subn(lambda m: m.group(1) + f'"{found}"', text, count=1)
        if n != 1:
            lines.append(f"  (not written: the entry is not laid out as bump expects)")
    if write:
        with open(CATALOGUE, "w", encoding="utf-8") as f:
            f.write(text)
    print("\n".join(lines) if lines else "Every upstream is at the version the catalogue pins.")


def bundle_types(path):
    """The types of a packed bundle, each with its param names."""
    with tarfile.open(path, "r:gz") as tar:
        header = json.load(tar.extractfile("bundle.json"))
        out = {}
        for entry in header["types"]:
            member = tar.extractfile(entry["file"])
            schema = json.load(io.TextIOWrapper(member, encoding="utf-8")) if member else {}
            out[entry["type"]] = sorted(p["name"] for p in schema.get("params", []))
        return header, out


def diff(old, new):
    _, before = bundle_types(old) if os.path.exists(old) else ({}, {})
    header, after = bundle_types(new)
    label = f"{header['target']}/{header['name']}:{header['version']}"
    added = sorted(set(after) - set(before))
    removed = sorted(set(before) - set(after))
    changed = sorted(t for t in set(after) & set(before) if after[t] != before[t])
    print(f"### {label}\n")
    print(f"{len(after)} types: {len(added)} added, {len(removed)} removed, "
          f"{len(changed)} with changed params.\n")
    for title, names in (("Added", added), ("Removed", removed)):
        if names:
            shown = ", ".join(f"`{n}`" for n in names[:50])
            more = f" and {len(names) - 50} more" if len(names) > 50 else ""
            print(f"- {title}: {shown}{more}")
    for name in changed[:50]:
        gained = sorted(set(after[name]) - set(before[name]))
        lost = sorted(set(before[name]) - set(after[name]))
        print(f"- `{name}`: +{', '.join(gained) or '-'} / -{', '.join(lost) or '-'}")
    print()


def main(argv):
    if not argv:
        sys.exit(__doc__)
    command, args = argv[0], argv[1:]
    if command == "matrix":
        matrix()
    elif command == "seed" and len(args) == 3:
        seed(*args)
    elif command == "show" and len(args) == 3:
        show(*args)
    elif command == "bump":
        bump("--write" in args)
    elif command == "diff" and len(args) == 2:
        diff(*args)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
