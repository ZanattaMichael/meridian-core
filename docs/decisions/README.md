# Decision records

Most of Meridian was built quickly by parallel pieces of work, and each made
design choices on its own, listed them in a pull request body, and merged. This
directory gathers the ones that change what a user sees, one short record each:
the context, the choice, the alternatives, and whether the owner has accepted
it.

A record's status is **proposed** until the owner accepts it or changes it. A
choice that is changed becomes an issue, and the record is updated to say so
and to point at the issue. An accepted record is a fact the reference pages
can rely on.

The [plans](../plans/README.md) say what was designed; the records say what
was decided while building. Where a plan and a record differ, the record
describes the code.

## Adding a record

A design choice made in a pull request gets a record in the same pull request:
copy the shape of any file here, number it after the last, and link it from the
page it affects. The pull request template asks for it.

## Records

| Record | Area | Status |
|---|---|---|
| [A notify's `on` accepts only `changed`](0001-notify-on-accepts-only-changed.md) | Language | proposed |
| [`metadata.labels` select documents and are not passed to plugins](0002-labels-select-documents-only.md) | Language | proposed |
| [With no data given, editors treat every `when` as true](0003-editors-treat-every-when-as-true.md) | Language | proposed |
| [A value placed by interpolation is always literal text](0004-placed-values-are-literal.md) | Language | proposed |
| [Packer sits under infrastructure as code](0005-packer-is-infrastructure-as-code.md) | Categories | proposed |
| [cloud-init sits under configuration management](0006-cloud-init-is-configuration-management.md) | Categories | proposed |
| [Containerization accepts both document kinds](0007-containerization-accepts-both-kinds.md) | Categories | proposed |
| [`compile`, `plan` and `apply` do not run compile rules](0008-compile-rules-are-a-separate-command.md) | Compile rules | proposed |
| [`no-override` allows restating a value and `final` does not](0009-no-override-allows-restating.md) | Compile rules | proposed |
| [Group and layer paths are case-sensitive](0010-group-paths-are-case-sensitive.md) | Compile rules | proposed |
| [There is no central module index](0011-no-central-module-index.md) | Modules | proposed |
| [One version of each module per project](0012-one-version-of-a-module-per-project.md) | Modules | proposed |
| [Registry logins are read only from Docker's `auths`](0013-registry-logins-from-docker-auths-only.md) | Modules | proposed |
| [The lock file does not record who signed a module](0014-lock-file-does-not-record-the-signer.md) | Modules | proposed |
| [Plugin signatures default to `require` in a release and `off` from source](0015-plugin-signatures-require-in-a-release.md) | Releases | proposed |
| [The Windows named pipe can be opened for reading by any local account](0016-windows-pipe-readable-by-local-accounts.md) | Releases | proposed |
| [A dispatch of the release workflow at a `v*` tag is a release](0021-a-dispatch-at-a-tag-is-a-release.md) | Releases | proposed |
| [Project settings are keyed by the plugin's short names, and the environment wins](0022-project-settings-named-by-the-plugin-and-the-environment-wins.md) | Plugins | proposed |
| [cloud-init's `apply` does not touch a machine](0017-cloud-init-apply-touches-no-machine.md) | Targets | proposed |
| [Helm's plan does not detect live drift](0018-helm-plan-does-not-detect-live-drift.md) | Targets | proposed |
| [Podman output is Quadlet units only](0019-podman-output-is-quadlet-units-only.md) | Targets | proposed |
| [Terraform import renames colliding names](0020-terraform-import-renames-colliding-names.md) | Targets | proposed |
