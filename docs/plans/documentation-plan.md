# Meridian — Documentation Plan

> Status: **accepted**. Originally authored in issue #3; this file is the canonical copy.
> Changes to this plan are made by pull request against this file, not by editing the issue.

This plan maps documentation directly onto the architecture and guarantees established in the design and testing plans. The governing principle: **every place the system has a documented capability gap, workaround, or hard limit (per the capability matrix) must have a corresponding, equally visible doc** — this project's core risk is users trusting a "unified DSL" claim more than the underlying tools actually allow, so documentation is where that trust gets calibrated correctly or doesn't.

---

## 1. Audiences

Documentation should be organized around who's reading, not around internal package structure — these are three different mental models and mixing them in one doc set is how projects end up with a "reference manual nobody can learn from" problem.

| Audience | Needs | Primary entry point |
|---|---|---|
| **New user** (evaluating or just starting) | "What is this, does it fit my case, can I get something running in 10 minutes" | Quickstart + Concepts |
| **Practicing author** (writing IR day to day) | "How do I express X," "why did my compile fail," "what does target Y actually support" | DSL Reference + Capability Matrix + Troubleshooting |
| **Plugin/SDK developer** (extending Meridian) | "How do I build a new target plugin," "what does the SDK guarantee me" | SDK Guide + Architecture Reference |
| **Operator** (running Meridian in CI/production) | "How do I sequence Infrastructure→ResourceSet safely," "what does drift/staleness mean for me," "how do secrets actually flow" | Operations Guide |
| **Contributor** (working on Meridian core) | "How is the repo laid out," "what are the testing tiers," "what's the release process" | Contributor Guide (internal-facing) |

---

## 2. Documentation Set Structure

```
docs/
├── index.md                      # what Meridian is, who it's for, honest scope statement
├── quickstart/
│   ├── install.md
│   ├── first-resourceset.md      # package/service example, single target
│   └── first-two-stage.md        # Infrastructure -> ResourceSet handoff example
├── concepts/
│   ├── ir-overview.md            # kind: Infrastructure vs ResourceSet
│   ├── resolution.md             # hierarchy/merge model
│   ├── conditions-and-facts.md   # when/gather/prune, static-output guarantee
│   ├── dependencies.md           # dependsOn vs notifies, edge kinds
│   ├── capability-model.md       # what "capability" means, how to read the matrix
│   └── two-stage-architecture.md # Infrastructure/ResourceSet split, why it exists
├── reference/
│   ├── dsl/
│   │   ├── infrastructure.md
│   │   ├── resourceset.md
│   │   ├── resource-types.md     # universal resource type catalog
│   │   ├── dependson.md
│   │   ├── notifies.md
│   │   ├── when-and-runtimewhen.md
│   │   └── secrets.md
│   ├── cli/
│   │   ├── meridian-plan.md
│   │   ├── meridian-apply.md
│   │   ├── meridian-validate.md
│   │   └── meridian-resolve.md
│   └── capability-matrix.md      # generated, not hand-maintained (see Section 5)
├── targets/
│   ├── ansible.md
│   ├── puppet.md
│   ├── chef.md
│   ├── dsc-classic.md
│   ├── dsc-v3.md
│   └── terraform-arm.md
├── operations/
│   ├── orchestration-sequencing.md
│   ├── secrets-handoff.md
│   ├── drift-and-staleness.md
│   ├── ci-integration.md
│   └── upgrade-guides/
├── sdk/
│   ├── plugin-architecture.md
│   ├── writing-an-emitter.md
│   ├── capabilities-contract.md  # what claiming a capability obligates you to
│   └── reference-plugin-walkthrough.md
├── troubleshooting/
│   ├── common-compile-errors.md
│   ├── cycle-detection.md
│   ├── unsupported-resource-type.md
│   └── dangling-dependency.md
└── contributing/
    ├── repo-layout.md
    ├── testing-tiers.md
    ├── adding-a-target.md
    └── release-process.md
```

---

## 3. Content Principles

1. **Capability honesty over marketing clarity.** Every target doc (`targets/*.md`) must open with a "what this target does and does not support" summary before any usage example — not buried at the bottom. This directly follows from the design plan's "no silent capability loss" principle; the docs are where that principle either holds or quietly gets abandoned under pressure to look feature-complete.
2. **Every workaround gets a named, linked explanation.** DSC's synthetic-notify hash-file mechanism and Terraform's `terraform_data` replacement-based notify are not implementation details — they change what a user sees in `dsc` output or a `terraform plan`. Both need a dedicated doc section (linked from `notifies.md` and from the respective target doc) explaining *why* the output looks the way it does, so a user doesn't file a bug report against expected behavior.
3. **Examples are runnable fixtures, not prose-only.** Wherever possible, DSL reference examples should be the *same* fixtures used in Tier 3 contract tests (`fixture_package_service.yaml`, `fixture_notify_changed.yaml`, etc.) — pulled into docs via include/transclusion, not copy-pasted. This keeps docs from drifting out of sync with what the system actually does, since a fixture change that breaks a doc example is caught by the doc build, not discovered by a user.
4. **Errors are documented before they're needed.** The troubleshooting section should be written in parallel with the error-message-quality tests (Section 8 of the testing plan), not after — each documented error message should link to the exact test that pins its wording, so the two stay in sync.
5. **No orphaned concepts.** Every field in the IR reference has a corresponding concept doc explaining *why* it exists (e.g. `notifies.md` links back to `concepts/dependencies.md`'s explanation of why `dependsOn` and `notifies` are different edge kinds) — reference docs answer "what," concept docs answer "why," and both are needed.

---

## 4. Priority Order (What Gets Written First)

Documentation should track the build sequencing milestones from the design plan, not lag behind them:

| Build milestone | Docs that must land alongside it |
|---|---|
| `ir` + `resolve` + `graph` core | `concepts/ir-overview.md`, `concepts/resolution.md`, `concepts/dependencies.md` |
| First hardcoded Ansible emitter | `quickstart/first-resourceset.md`, `targets/ansible.md`, `reference/dsl/resourceset.md` |
| Plugin boundary extraction | `sdk/plugin-architecture.md`, `sdk/writing-an-emitter.md` — written *while* extracting, since that's when the SDK's real rough edges are freshest |
| Puppet + Terraform emitters added | `targets/puppet.md`, `targets/terraform-arm.md`, `concepts/two-stage-architecture.md`, `quickstart/first-two-stage.md` |
| DSC-classic, DSC-v3, Chef added | `targets/dsc-classic.md`, `targets/dsc-v3.md`, `targets/chef.md`, plus the full `reference/capability-matrix.md` becomes meaningful for the first time (5+ targets is when a matrix actually earns its place over prose) |

Rationale: writing target docs before a target plugin exists produces documentation that describes intent, not behavior — better to lag docs by one milestone per target than to publish aspirational capability claims that Tier 3 contract tests haven't verified yet.

---

## 5. The Capability Matrix Must Be Generated, Not Hand-Maintained

This is the single highest-risk doc in the set, because it's also the thing most likely to silently go stale. Given that:

- `reference/capability-matrix.md` should be **generated at build time** directly from each plugin's `Capabilities{}` struct (Section 4.6 of the testing plan already runs a self-consistency check against this — the doc generator should consume the same source of truth).
- CI should fail the docs build if a plugin's declared capabilities and the checked-in matrix diverge — mirroring the "schema/doc drift" non-functional test from the testing plan (Section 8), extended to cover this doc specifically.
- Each cell in the generated matrix should link to the relevant `targets/*.md` section explaining *how* that capability is achieved (native/synthetic) — a flat true/false table without that link is not actually useful to a practicing author trying to decide whether to use `notifies` against DSC.

---

## 6. Format & Tooling

- **Format**: Markdown throughout, consistent with the repo's existing design/testing plan docs — no new toolchain dependency for the doc source itself.
- **Static site generation**: a standard docs generator (e.g. Docusaurus, mkdocs-material, or Hugo) that supports include/transclusion (needed for the fixture-as-example principle in Section 3.3) and versioning (needed once Meridian itself has multiple released versions, given the orchestrator run-state schema upgrade concern from the design plan).
- **API/SDK reference**: generated from Go doc comments (`godoc`/`pkgsite`-style) for `pkg/sdk`, kept separate from the prose SDK guide — reference and guide serve different needs and shouldn't be forced into one document.
- **Versioning**: docs versioned alongside Meridian releases from the first tagged release onward, since IR schema changes or capability changes between versions (e.g., DSC v3 gaining a native notify primitive, per the open question in the design plan) would otherwise leave no way to read docs matching an older deployed version.
- **Search**: full-text search across the whole doc set is a hard requirement given the size of the target-specific + troubleshooting sections — this is not a nice-to-have once 5+ target docs and a growing troubleshooting section exist.

---

## 7. Ownership & Review

- **Target docs** (`targets/*.md`) should be reviewed by whoever owns that plugin — same reviewer discipline as the plugin's contract tests, since the doc is making a claim the tests are supposed to enforce.
- **Capability matrix** changes should require the same review as a `Capabilities{}` struct change in code — treat the generated doc as part of the plugin's public contract, not a separate artifact.
- **Concept docs** should be reviewed for consistency with the design plan itself — if a concept doc and the design plan diverge, that's a signal either the implementation drifted from the design or the design plan needs updating; either way it shouldn't be silently resolved by only fixing the doc.
- **Troubleshooting entries** should be added as a required step whenever a new error message is introduced or changed in core/plugin code (tie this into PR review checklist, not left to a later documentation pass).

---

## 8. Open Items to Resolve Before Writing Begins

- Which static site generator supports both fixture transclusion and versioned docs well enough to avoid a mid-project tooling migration.
- Whether target docs live in the main `meridian-core` repo or move out alongside plugins if/when the multi-repo plugin org split happens (Section 12 of the design plan) — docs-with-code co-location matters more for plugins than for core concepts, so this may end up split rather than uniform.
- Whether the SDK guide needs a "minimum viable plugin" tutorial separate from the reference-plugin walkthrough, aimed at someone who has never read the core architecture doc at all — worth user-testing with an actual third-party contributor before assuming the existing structure is sufficient.
