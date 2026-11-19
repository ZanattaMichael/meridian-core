# A lab is a matrix over lifecycle cases, and sweep reads the state

| | |
|---|---|
| Area | Testing |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | issue 236, `meridian lab` (parts 1 and 2) |
| Where it shows | [Running a lab of environments](../operations/lab.md), [`meridian lab`](../reference/cli.md#lab), [Machine-readable output](../reference/cli-json.md#lab-run-lab-list-lab-sweep) |

## The choice

- **Environments are Test files' lifecycle cases.** A `Lab` document adds only the matrix,
  exclusions, concurrency and expiry; provisioning, checks and destroy are the case's.
- **Matrix values are read as `meridian.lab.<axis>`**, not as data, and documents pick their
  cells with `when`. No document field becomes an expression.
- **One state directory and one run id per cell**: `lab/<lab>/<run>/<cell>-<case>`, run id
  `<lab run>-<cell>`. Cells share nothing, so they run in parallel threads over one set of
  plugins.
- **Every lifecycle environment writes `environment.json`** (documents, data, selector, run
  id, expiry). `lab sweep` destroys from it alone, for lab cells and kept test cases alike.
- **Expiry is the lab's**; a kept test case has none and is swept only with `--older-than`.
- **Meridian adds the lab's tags** (`meridian-lab-run`, `meridian-lab-cell`,
  `meridian-lab-expires`) to every resource whose type, as the target describes it, takes a
  `tags` mapping, before the target compiles. The target writes them through its own tag
  mechanism. A mapping for a resource that carries no tags on its cloud leaves them out, and
  still refuses the author's own.
- **Tag names are lowercase letters, digits and `-`**, the one form AWS tags, Azure tags and
  Google Cloud labels all take.
- **A sweep by tags comes later**, for when the state directory is gone.

## Alternatives

- Matrix values as `data.<axis>`. Rejected: they would clash with a project's own data and
  reach plugins.
- One process per cell. Rejected: each would start every plugin again.
- Tags named `meridian.lab/run`. Rejected: Azure tag names refuse `/`, and Google Cloud labels
  refuse `.` and `/`.
