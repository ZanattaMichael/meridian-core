# Packer sits under infrastructure as code

| | |
|---|---|
| Area | Categories |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | PR 27 |
| Where it shows | [Packer](../targets/packer.md), [capability model](../concepts/capability-model.md) |

## Context

There are four plugin categories. Packer builds images, which is none of them.

## The choice

Packer takes an Infrastructure document and is listed under infrastructure as code, the nearest category, so that a document for it looks like one for Terraform.

## Alternatives

Add an image-building category with its own document kind.
