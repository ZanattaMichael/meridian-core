# A nightly is a rolling pre-release of `main`

| | |
|---|---|
| Area | Releases |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the `nightly` workflow |
| Where it shows | [Releasing](../contributing/releasing.md#nightly), [Install](../quickstart/install.md#nightly), [Modules](../reference/modules.md) |

## Context

The owner asked for a nightly build. The [release plan](../plans/release-and-versioning.md#52-channels) had ruled one out.

## The choice

- `nightly.yml` calls `release.yml` each night `main` changed, and replaces one GitHub pre-release and tag, both named `nightly`.
- Version `<next patch>-nightly.<YYYYMMDD>.g<sha7>`: sorts after the release it was built from, before the next. No `+` build metadata, which some download URLs mangle.
- A module's `spec.meridian` checks a pre-release core as its release, so `>=0.1` admits `0.1.1-nightly.…`.
- Build provenance only. No Sigstore signature, so the install scripts' signer check (a `v*` tag of `release.yml`) never passes a nightly as a release. No image, no package-manager listings, nothing outside the repository's releases.
- Real-tool tests are not repeated: they already run on every push to `main` that touches what they test.

## Alternatives

A dated pre-release per night (`v0.1.1-nightly.20261119`): keeps history, but fills the releases page and needs pruning.

Pushing `ghcr.io/…/meridian:nightly`: left out until the owner asks; the registry is outside the repository.
