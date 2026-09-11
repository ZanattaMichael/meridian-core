# Contributing to Meridian

The full model — plugin tiers, ownership, capability disputes, triage — is in
[`docs/plans/governance.md`](docs/plans/governance.md). This is the short version.

## Before you write a target plugin

Read [`docs/plans/governance.md`](docs/plans/governance.md) and the security contract in
[`docs/plans/security.md`](docs/plans/security.md#6-plugin-sdk-secrets-contract) first.
The obligations attached to a capability claim are much easier to meet while writing the
plugin than to retrofit afterwards.

## The merge bar

**Tier 2 and Tier 3 tests must pass before merge.** Every plugin, every tier, including
experimental. Tier 3 is the cross-plugin contract suite, and it is what makes a
capability claim mean the same thing on your target as on every other one.

An experimental plugin may declare exemptions from individual Tier 3 fixtures. An
exemption is listed in the plugin manifest and rendered in the capability matrix as
"not verified". It is never silently absent.

**Tier 4 must pass before a plugin is listed as Supported.** Containerized integration
tests against the real target CLI.

The tiers are defined in [`docs/plans/testing-plan.md`](docs/plans/testing-plan.md).

## Capability claims

If you declare `Capabilities{NativeNotify: true}`, the shared contract suite must pass
against your plugin. If a reviewer contests a claim, they write a fixture that would fail
if the claim were false, and the test decides. Ties break toward the weaker claim.

## Changes needing a second reviewer

Regardless of plugin tier:

- Adding or changing a `SecretsBackend` or `SecretsAtRest` declaration.
- Adding or moving a call to `Reveal()`. One call site per plugin is the limit.
- Touching `exec` escaping, or any path building a shell command string.
- Changing what reaches the run-state store.

## Documentation

A new or changed error message needs a troubleshooting entry in the same pull request,
not a later documentation pass. A changed capability needs the capability matrix
regenerated.

Relative links and heading anchors are checked in CI by `tools/check_links.py`. Run it
locally before pushing:

```
python3 tools/check_links.py
```

## Security reports

Do not open an issue. See [`SECURITY.md`](SECURITY.md).
