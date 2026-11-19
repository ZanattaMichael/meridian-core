# Failure causes

The word a [resource failure](state.md#resource-failures) carries for why the
resource failed. This page is generated: `crates/meridian-cli/tests/failure_causes.rs`
writes it from `meridian_sdk::failure::Cause` and fails when it is out of date.

A word never changes and is never reused. A plugin picks the word from its
tool's structured output; a word a newer plugin sends that this `meridian`
does not know reads as `unknown`.

| Cause | Meaning |
|---|---|
| `unreachable` | the machine or the service could not be reached |
| `auth-failed` | it was reached, and refused the identity |
| `invalid-config` | the tool refused the configuration before changing anything |
| `dependency-missing` | something the resource needs is not there: a provider, a module, a package source, an image |
| `resource-failed` | the tool tried to change the resource and could not |
| `dependency-failed` | not tried, because a resource it depends on failed |
| `timed-out` | the resource did not finish in the time the tool allows it |
| `reboot-required` | the change was made and needs a restart to take effect |
| `tool-crashed` | the tool itself stopped unexpectedly |
| `unknown` | the tool's report says it failed and not why |
