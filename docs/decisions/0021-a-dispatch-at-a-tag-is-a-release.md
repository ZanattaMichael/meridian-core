# A dispatch of the release workflow at a `v*` tag is a release

| | |
|---|---|
| Area | Releases |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the `cut release` workflow |
| Where it shows | [Releasing](../contributing/releasing.md#cutting-a-release) |

## Context

The owner asked for a button in the Actions tab that cuts a release. A workflow can make and push the tag with its own token, but GitHub starts no workflows for pushes made with that token, so the tag alone would not start the `release` workflow. The `cut release` workflow has to start it by dispatching it at the tag.

The `release` workflow had one rule for publishing: a tag was pushed, and the repository variable `MERIDIAN_RELEASE_PUBLISH` is `true`. A dispatch is not a push, so that rule would have made every button press a rehearsal.

## The choice

A `workflow_dispatch` of `release` whose ref is a `v*` tag publishes, with no variable. Pressing the button (or choosing the tag under Run workflow) is the decision to release; asking for a second switch would only be the same decision stated twice. A tag pushed by hand keeps the variable, because a push can be a habit and the variable is the confirmation.

The irreversible step, the `publish` job, still sits behind the `release` environment, where an approval can be required. The tag and `Cargo.toml` must agree whichever way the run started.

## Alternatives

Keeping the variable for every run, and having `cut release` fail unless it is set. One more setting to forget, for no added safety: the environment already provides the approval.

A personal access token or a GitHub App for the tag push, so that the pushed tag starts `release` the ordinary way. A long-lived secret to make, store and rotate, which the pipeline had so far avoided.
