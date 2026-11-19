#!/usr/bin/env python3
"""Writes a large Meridian project for the fake target, for scale tests.

    tools/lab/generate.py --resources N --endpoints M --stages S [--seed K] OUT

OUT gets `stages/stage-<i>.yaml` (S Infrastructure documents), and
`hosts/host-<j>.yaml` (M ResourceSets, each on its own endpoint, whose
`targetHost` is an output of one stage). Endpoints take 2 resources each and
the stages share the rest; within a stage every third resource refers to the
one before it. The same arguments and seed write the same files.
"""

import argparse
import random
import sys
from pathlib import Path

PER_ENDPOINT = 2


def stage(i: int, count: int, rng: random.Random) -> str:
    lines = [
        "apiVersion: meridian/v1",
        "kind: Infrastructure",
        "metadata:",
        f"  name: stage-{i}",
        "spec:",
        "  target: fake",
        "  resources:",
    ]
    for r in range(count):
        lines += [f"    - id: r{r}", "      type: file", "      params:"]
        if r % 3 == 2:
            lines.append(f'        content: "${{r{r - 1}.content}}-{r}"')
        else:
            lines.append(f"        content: v{rng.randrange(10**6)}")
    lines += [
        "  outputs:",
        "    - name: address",
        f"      value: 10.{i // 250}.{i % 250}.1",
    ]
    return "\n".join(lines) + "\n"


def host(j: int, stages: int, rng: random.Random) -> str:
    lines = [
        "apiVersion: meridian/v1",
        "kind: ResourceSet",
        "metadata:",
        f"  name: host-{j}",
        "spec:",
        "  target: fake",
        "  targetHost:",
        "    from: infrastructure",
        f"    ref: stage-{j % stages}",
        "    output: address",
        "  resources:",
    ]
    for r in range(PER_ENDPOINT):
        lines += [
            f"    - id: f{r}",
            "      type: file",
            "      params:",
            f"        content: host-{j}-{rng.randrange(10**6)}",
        ]
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--resources", type=int, required=True)
    parser.add_argument("--endpoints", type=int, default=0)
    parser.add_argument("--stages", type=int, default=1)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("out", type=Path)
    a = parser.parse_args()
    if a.stages < 1:
        parser.error("--stages must be at least 1")
    in_stages = a.resources - PER_ENDPOINT * a.endpoints
    if in_stages < a.stages:
        parser.error(
            f"--resources must be at least {PER_ENDPOINT} per endpoint and 1 per stage "
            f"({PER_ENDPOINT * a.endpoints + a.stages} here)"
        )
    rng = random.Random(a.seed)
    for sub in ("stages", "hosts"):
        (a.out / sub).mkdir(parents=True, exist_ok=True)
    base, extra = divmod(in_stages, a.stages)
    for i in range(a.stages):
        count = base + (1 if i < extra else 0)
        (a.out / "stages" / f"stage-{i}.yaml").write_text(stage(i, count, rng))
    for j in range(a.endpoints):
        (a.out / "hosts" / f"host-{j}.yaml").write_text(host(j, a.stages, rng))
    print(
        f"wrote {a.stages} stages and {a.endpoints} endpoints, "
        f"{a.resources} resources, to {a.out}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
