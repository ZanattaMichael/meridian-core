# The common infrastructure types are a small core, checked once and mapped by each target per cloud

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | `meridian_sdk::infra` (the vocabulary, `check_common`, `lower`); `plugins/terraform/src/common.rs`, which OpenTofu shares, and `plugins/bicep/src/common.rs` (the mappings) |
| Where it shows | [The common infrastructure types](../reference/dsl/common-infrastructure-types.md), the [conformance matrix for them](../reference/infra-conformance-matrix.md), the Terraform and Bicep target pages |

## Context

Every infrastructure as code target was native passthrough: the same machine
was written `aws_instance` for Terraform on AWS,
`azurerm_linux_virtual_machine` for Terraform on Azure and
`Microsoft.Compute/virtualMachines@...` for Bicep, and a document written for
one could not compile for another. The owner asked for common types, defined
once as a middle layer and translated into each target's native resource
(issue 151).

## The choice

**Five types, and no more for now:** `network`, `subnet`, `firewall`,
`address`, `vm`. Each has a one-to-one resource on every cloud mapped. Disks,
object stores and DNS records come next; load balancers, databases and managed
Kubernetes wait until these have shown what works.

**The vocabulary lives in the SDK**, beside the six configuration management
types. `check_common` reads params, kinds, required params, reference params
and the fixed shapes (`size`, `image`, firewall `rules`) the same way for every
target, before the target applies its own rules. So the same mistake is the
same refusal on every cloud, which the generated matrix holds to.

**A target lowers the common types to its own before anything else.**
`infra::lower` replaces each common resource with the native resources a
target's mapping gives, rewrites every read of a promised output
(`${vm.publicAddress}`) to the tool's own expression, and hands the target a
tree of native types only. The rest of the compile is unchanged, so references,
ordering, secrets and validation work on common resources for free. The native
resource that stands for a common one keeps its id, so `${id}`, `dependsOn`
and the tool's own attributes still work; helpers take ids derived from it,
and a clash with the document's own ids is refused.

**Outputs are promised, not attributes.** `id` on every type, `address` on an
address, `privateAddress` and `publicAddress` on a vm. A read of a promised
output that one cloud cannot give is refused with the reason (an Azure vm has
no public address without an `address`); a read of anything else is the
tool's own and is handed over as written.

**Sizes and images are tables, and the text form passes through.**
`{cpus, memoryGb}` becomes the smallest size in the cloud's table that fits,
and `{family, version}` one of four images by publisher. The choice is in the
compiled file. A size or image written as text is the cloud's own and is not
checked. A size bigger than the table is refused rather than rounded down.

**Where a cloud has no equivalent, the target refuses.** AWS has no location
per resource (the provider's region decides), no name on an elastic IP and no
`user` (the image decides it). Azure has no public subnets, no tags or
location on a subnet, one security group per network interface, and no Linux
machine without a key. Each is `unsupported-param` or `missing-param` with the
reason, and the matrix records it.

**OpenTofu takes Terraform's mapping unchanged**, since both read the same
providers; a common resource compiles to the same blocks for either.

**Terraform chooses the cloud from `spec.settings.cloud`, or from the one
cloud whose provider the document configures.** Azure's resource group and
default location are `spec.settings.resourceGroup` and `.location`, so a
document says them once. Bicep is always Azure and needs none.

**Rules and routes are separate resources on Terraform.** Inline
`security_rule`, `route` and `ingress` blocks are "attributes as blocks" in
the providers, which in Terraform's JSON syntax must list every attribute;
the separate rule, route and association resources avoid that and are the
providers' current advice.

## Alternatives

**A larger vocabulary from the start** (load balancers, databases, Kubernetes
clusters). Rejected: these differ most between clouds, and the systems that
tried it narrowed or stopped (Pulumi's `@pulumi/cloud`, archived 2024).

**No vocabulary, only composites a team writes.** The language already allows
a `vm` composite with a variant per target, but every team would write its own
and nothing would be checked across clouds. Crossplane takes this road.

**Mapping inside each target's transform, without a shared lowering step.**
Each target would rewrite output reads and mint helper ids its own way, and
the promise of the same outputs everywhere would rest on each plugin.

**Silently rounding a size, or picking an image by name only.** A machine
smaller than asked, or an AMI from an account that is not the publisher's, is
exactly the silent loss the language refuses elsewhere.

## Not yet decided

- Which targets come next: CloudFormation, ARM and Pulumi need mappings of
  their own.
- Whether GCP is mapped on Terraform, and with which sizes.
- Whether a plan should also print the size and image chosen as a note, beyond
  the compiled file.
