# An analyzer reads the compiled files on disk, and every finding points at the document

| | |
|---|---|
| Area | Runs |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Analyzer plugins: scan the compiled files with Checkov, Trivy or any scanner" (issue 116) |
| Where it shows | [Analyzers](../reference/analyzers.md), [`--analyze`](../reference/cli.md#options), [Running safely](../concepts/run-safety.md#scanning-the-compiled-files-first), [Reports](../reference/reporting.md#sarif), `meridian_orchestrator::analyze` |

## Context

Policy checks are Rego over Meridian's own view of a stage. The large
libraries of security and compliance rules (Checkov, Trivy, KICS, Infracost)
are written against each tool's native format, and Meridian's output is that
format, so they apply unchanged; nothing ran them. The pattern for an
external program beside the core, a named program with a small protocol
that fails closed, had been built three times (`meridian-policy-<name>`,
`meridian-backend-<name>`, `meridian-notify-<name>`). The one new piece was
where a finding should point: a scanner reports a line of `main.tf.json`,
which nobody wrote.

## The choice

An analyzer is a fourth kind of program, `meridian-analyze-<name>`, and it
differs from a policy engine in what it is given: **the compiled files on
disk**, in a directory of their own that exists only while the analyzer
runs, with a small JSON document on standard input naming the stage, the
directory and the settings. A policy engine is given the whole input on
standard input and nothing on disk because its input holds secrets; a
scanner reads files, so the files are written, and removed after, whatever
happens.

A finding is **traced to the document** by Meridian, not by the emitters.
The analyzer may name the resource; the reference analyzers take the last
part of the scanner's resource name (`aws_s3_bucket.logs`), which is the
document's id wherever the emitter could keep it. When it does not, or
names something the document does not have, the compiled line is traced to
the last resource whose id appears as a whole word on or above it. The
compiler then knows the line of the document that resource starts on. This
needs no side file from any emitter, and a wrong guess only changes which
document line a finding points at.

An analyzer runs on **`compile` as well as `plan` and `apply`**, as soon as
the stage has compiled, because scanning needs nothing but the files and a
compile is the cheapest check a pull request can run. A compile or a plan
goes on past a failed stage so that one run shows everything there is to
find; an apply analyzes every stage it can in its first pass and applies
nothing when one fails, as it does with a policy.

A finding at or above **`--fail-on`, `high` by default**, fails the stage
with the reason `analysis-failed`; an analyzer that could not look fails it
with `analyzer-failed`, whatever `--fail-on` says, because a scanner that
was not run has not passed anything. An accepted finding, named in a
**baseline file** by rule and optionally analyzer, resource, stage and
target, is still reported and fails nothing.

**Every finding is an event**, `finding.reported`, carrying the rule, the
severity, the compiled place, the resource, the document's place, and the
analyzer's message. This is the one place a run's record holds text a tool
wrote. The rule that an event never holds a value a tool produced stands;
the message is the analyzer's description of its rule, passed through the
run's masking of every value it knows to be sensitive, and the protocol
tells an analyzer to name the rule and never quote the file. Without the
message a report could say that `CKV_AWS_18` fired and not what that means,
and SARIF, which the reporting work was waiting on, needs the text. With
findings as events, SARIF, the findings tables in Markdown and HTML, the
JUnit output and the pipeline annotations needed no new path.

Checkov gives a check a severity only with a Prisma Cloud key, so the
Checkov analyzer reports a failed check with none at `high`, which keeps
Checkov's own meaning (it fails on any failed check) under the default
threshold; the setting `default-severity` changes it.

## Alternatives

- **Emitters writing a side file** mapping output positions to document
  positions, as the issue suggested. Every one of eighteen emitters would
  have to be changed and kept right, for a mapping the resource id already
  gives in nearly every format. The whole-word search is a heuristic, and is
  described as one.
- **Analyzers only on `plan` and `apply`**, as policies are. A policy needs
  the tool's preview; a scanner does not, and a compile on a pull request is
  where a finding is cheapest to fix.
- **Findings as counts only**, as policy results are recorded. A finding
  without its rule and place cannot be shown on a pull request, and that is
  what the feature is for. Issue 102 asks the same of policy findings.
- **Failing on nothing by default.** A scanner's job is to stop a bad
  change; a default that only reports would make `--analyze` on `apply`
  decorative. `--fail-on none` is one word away.
- **Baselines shipped with each reference analyzer.** A list of findings
  Meridian's boilerplate always trips is a per-emitter fact worth keeping,
  but it was not measured, so none ships; a project's baseline file does the
  same job in the open.

## What the owner may change

The default of `--fail-on`; whether the finding's message belongs in the
record; the baseline file's name and shape; whether `compile` should
analyze; the severity Checkov's failed checks are given.
