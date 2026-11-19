# A lifecycle case provisions, verifies and always destroys

| | |
|---|---|
| Area | Testing |
| Status | **proposed**: made during the work, not yet accepted or changed by the owner |
| Made in | issue 235, the lifecycle level of `meridian test` |
| Where it shows | [Testing documents and modules](../reference/testing.md#lifecycle-cases), [`meridian test`](../reference/cli.md#test), [Machine-readable output](../reference/cli-json.md#test) |

## The choice

- **A third level, `run: lifecycle`**, in the same `Test` document: `provision` (apply),
  `verify` (the case's `expect` and `assert`, and `drift: none` as a second plan), `destroy`.
- **Destroy always runs**, after a failed provision, a failed verify or a timeout. Only
  `--keep` skips it.
- **One JUnit testcase per phase** (`<case>: <phase>`); a phase that did not run is `skipped`.
- **A target that cannot destroy is an error before anything is applied**, naming the target.
- **The scratch directory is removed only after a destroy that succeeds.** A failed destroy
  or `--keep` leaves it; the next run of the case refuses to start over it rather than
  throw away the record of what is still out there.
- **`timeout` covers provision and verify together.** Destroy gets the same limit per tool.
- **`break` and `diagnose` are refused** until the fault catalogue (issue 237) exists.
- **Checks move to verify.** `expect` and `assert` read the apply; `stages.<name>.outputs`
  holds an applied document's outputs, a sensitive one masked.

## Alternatives

- A separate `kind: Lab` document. Rejected: the data, selectors and checks are the
  compile and plan case's; the matrix runner (issue 236) can run these cases.
- Reusing the scratch directory, emptied, as plan cases do. Rejected: it would forget what a
  failed destroy left behind.
