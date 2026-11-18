# Package-manager listings are sent by a workflow and start private

| | |
|---|---|
| Area | Releases |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the `package managers` workflow |
| Where it shows | [Releasing](../contributing/releasing.md#package-managers), [Install](../quickstart/install.md) |

## Context

The owner asked for the Homebrew, Scoop and winget listings to be deployed but held back, out of a concern that they need code signing, which costs money. None of the three package managers checks a code signature; the warnings signing removes appear only for archives downloaded in a browser. The owner still wanted a way to prepare the listings without making them public.

## The choice

A `package managers` workflow sends a published release's manifests to repositories of the owner's: the tap and the bucket as repositories that can be private, and winget as a branch of the owner's fork, with the pull request to `microsoft/winget-pkgs` opened only on request. Going public is then a visibility switch on each repository and a button for winget, with no files changing. A tap or bucket with history gets a draft pull request rather than a push to its default branch.

The workflow needs a personal access token, because a run's own token cannot write to other repositories. A GitHub App would avoid a token that expires, at the cost of an app to register and install.

## Alternatives

Keeping the tap and the bucket inside `meridian-core` (`brew tap` and `scoop bucket add` accept any repository URL). No new repositories, but every user's tap would clone the whole project, and the files could not be private while the project is public.

Waiting for code signing before listing anything. Nothing in the three managers depends on it; see the releasing guide.
