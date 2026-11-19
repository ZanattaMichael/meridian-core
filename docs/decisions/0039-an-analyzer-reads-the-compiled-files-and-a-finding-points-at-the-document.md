# An analyzer reads the compiled files on disk, and every finding points at the document

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Analyzer plugins: scan the compiled files with Checkov, Trivy or any scanner" (issue 116) |
| Where it shows | [Analyzers](../reference/analyzers.md), [`--analyze`](../reference/cli.md#options), [Running safely](../concepts/run-safety.md#scanning-the-compiled-files-first), [Reports](../reference/reporting.md#sarif), `meridian_orchestrator::analyze` |

## Context

Checkov, Trivy and similar scanners already have large rule sets for each tool's native
format, and Meridian's output is that format. Nothing ran them. A scanner reports a line of
`main.tf.json`, which nobody wrote, so a finding also needs to point back at the document.

## The choice

- **A new kind of program, `meridian-analyze-<name>`.** It reads the compiled files from a
  temporary directory, with a small JSON input on standard input. The directory is removed
  afterwards.
- **Meridian traces a finding to the document, not the emitters.** It uses the resource the
  analyzer names. Failing that, it uses the last resource id written on or above the line.
  No emitter changes are needed.
- **Analyzers run on `compile`, `plan` and `apply`.** Compile and plan keep going past a
  failed stage. Apply checks every stage it can before applying any.
- **Fail closed.** A finding at `--fail-on` (default `high`) fails the stage. An analyzer
  that cannot run fails it too.
- **A baseline file accepts known findings.** They are still reported, but fail nothing.
- **Every finding is a `finding.reported` event, message included.** The message is masked.
  It is the only tool text in the record, because a report or SARIF without it cannot say
  what fired.
- **Checkov's failed checks without a severity count as `high`**, matching Checkov's own
  behaviour. `default-severity` changes it.

## Alternatives

- **Emitters write a source map.** All eighteen emitters would need changing, and resource
  ids already give the mapping in nearly every format.
- **Only `plan` and `apply` analyze.** Scanners need no preview, and compile is the cheapest
  place to catch a finding.
- **Findings as counts only.** Then a finding cannot be shown on a pull request line.
- **Fail on nothing by default.** That would make `--analyze` on `apply` decorative.
- **Ship baselines per analyzer.** None were measured, so none ship.

## What the owner may change

The `--fail-on` default, keeping the message in the record, the baseline shape, analyzing on
`compile`, and the severity of Checkov's unrated checks.
