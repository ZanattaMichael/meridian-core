# Planning documents

These are the designs the code was built from. Each one began life as a GitHub issue
or a pull request and was moved here so it can be reviewed, versioned, and diffed like
code. Each file opens with its status: **accepted** says the owner signed it off,
**built** says the code landed, and the table below says which. Where a plan and the
reference pages differ, the reference describes the code; the design choices made while
building, which no plan settled, are the [decision records](../decisions/README.md).

Amend a plan by opening a pull request against the file. Do not edit the originating
issue — it is closed and is a historical record of how the plan was first stated.

| File | Source | Status |
|---|---|---|
| `design-plan.md` | #1 | accepted. Sections 11 and 12 describe the Rust implementation; earlier sections still describe parts of the Go prototype they were written for, and the reference pages are the authority |
| `testing-plan.md` | #2 | accepted |
| `documentation-plan.md` | #3 | accepted |
| `governance.md` | #4 | accepted |
| `migration-and-adoption.md` | #5 | accepted |
| `observability.md` | #6 | accepted |
| `release-and-versioning.md` | #7 | accepted |
| `security.md` | #8 | accepted |
| `run-safety.md` | written as a pull request; amends design plan §11.5 | built (pull request 60, closes #47); its decisions R1 to R7 await the owner |
| `reporting.md` | written as a pull request | built (pull request 70); the owner chose its direction on 2026-11-17 |
| `language-expansion.md` | pull request #59 | accepted by the owner on 2026-11-17; phases 1, 2 and most of 6a built |
| `modules-and-versioning.md` | written as a pull request | built (pull requests 62, 67, 83, 85, 131); not reviewed by the owner |

A plan that contradicts another plan is a bug in whichever one was written later. Raise
it as an issue rather than resolving it silently in one file.
