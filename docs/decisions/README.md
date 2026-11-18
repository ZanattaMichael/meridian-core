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
| [YAML tags other than `!!str` are refused, and `08` is text](0027-yaml-tags-are-refused-and-08-is-text.md) | Language | proposed |
| [Tool-wide settings live under `spec.settings`, not in a resource](0031-tool-wide-settings-live-under-spec-settings.md) | Language | proposed |
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
| [Project settings are keyed by the plugin's short names, and the environment wins](0023-project-settings-named-by-the-plugin-and-the-environment-wins.md) | Plugins | proposed |
| [A stop is asked of the plugin, and an interrupted run is not rolled back](0025-a-stop-is-asked-of-the-plugin-and-an-interrupted-run-is-not-rolled-back.md) | Runs | proposed |
| [cloud-init's `apply` does not touch a machine](0017-cloud-init-apply-touches-no-machine.md) | Targets | proposed |
| [Shared param kinds are checked once, strictly, for all six configuration management targets](0024-shared-param-kinds-are-checked-once-strictly.md) | Targets | proposed |
| [Helm's plan does not detect live drift](0018-helm-plan-does-not-detect-live-drift.md) | Targets | proposed |
| [Podman output is Quadlet units only](0019-podman-output-is-quadlet-units-only.md) | Targets | proposed |
| [Terraform import renames colliding names](0020-terraform-import-renames-colliding-names.md) | Targets | proposed |
| [A secret has one place on each containers target](0022-a-secret-has-one-place-on-each-containers-target.md) | Targets | proposed |
| [An error's kind is a fixed code that is never reused](0026-error-codes-are-fixed-words-and-never-reused.md) | Core | proposed |
| [A pipeline engine is shown the events and the report, and nothing formatted a second time](0033-annotations-come-from-events-and-the-report-only.md) | Reporting | proposed |
| [The MCP server is a subcommand of `meridian`, read-only, speaking the protocol itself](0035-the-mcp-server-is-a-read-only-subcommand.md) | Tooling | proposed |
| [A command's JSON result is one object, and a plan with changes exits 2 only when asked](0030-json-output-is-one-object-and-plan-exits-2-only-when-asked.md) | Core | proposed |
| [A refusal's rule is one of a registry, and the same mistake has the same name on every target](0034-a-refusal-rule-is-one-of-a-registry.md) | Core | proposed |
| [An apply from a saved plan applies that plan or nothing, and the file never holds a value](0038-an-apply-from-a-saved-plan-applies-that-or-nothing.md) | Orchestrator | proposed |
| [A test is a document of cases over compile and plan, and never a script](0037-a-test-is-a-document-of-cases-over-compile-and-plan.md) | Testing | proposed |
| [The common infrastructure types are a small core, checked once and mapped by each target per cloud](0040-common-infrastructure-types-are-a-small-core-mapped-per-cloud.md) | Targets | proposed |
| [A key reaches a host through an agent Meridian starts](0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md) | Targets | proposed |
| [A cloud credential reaches a tool in its environment](0029-a-cloud-credential-reaches-a-tool-in-its-environment.md) | Targets | proposed |
| [A configuration management target takes its tool's own types beside the six, and a colliding name is written `native/<name>`](0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md) | Targets | proposed |
| [A tool's published schema file is fetched over HTTPS, pinned by its hash, and the last copy stands in offline](0036-a-published-schema-file-is-fetched-over-https-pinned-by-hash-and-reused-offline.md) | Tooling | proposed |
| [Tool schemas travel as signed OCI bundles, chosen by the types the documents use and pinned by digest](0041-tool-schemas-travel-as-signed-oci-bundles-chosen-by-the-types-the-documents-use.md) | Tooling | proposed |
| [`adopt` is a field of the resource, and each target says how it adopts](0042-adopt-is-a-resource-field-and-a-target-says-how-it-adopts.md) | Language | proposed |
| [`meridian retarget` writes a new file from what the old tool reports](0043-retarget-writes-a-new-file-from-what-the-old-tool-reports.md) | Core | proposed |
