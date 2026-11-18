# Conformance matrix for the common infrastructure types

What happens to the same document on each target, and each cloud, that maps
the [common infrastructure types](dsl/common-infrastructure-types.md)
(`network`, `subnet`, `firewall`, `address`, `vm`). This page is generated:
`crates/meridian-cli/tests/infra_conformance.rs` compiles one small document
per row for every column and writes down how each compile came out, and it
fails when this page is out of date. So the page cannot drift from the code,
and a target cannot start accepting or refusing something without the change
showing here.

How to read a cell:

- **ok**: the document compiled.
- **ok, warns**: it compiled, with a warning the plan shows.
- a name such as `unsupported-param`: the target refused the document, and that
  is the rule the refusal carries, one of the [refusal rules](refusal-rules.md).
  The message names the resource, the line, and what the cloud does instead.

The **same** column says whether the columns are allowed to differ. A row
marked *yes* is a part of the vocabulary that must come out the same on every
cloud, and the test fails if one column breaks ranks. A row marked *no* is a
place where the clouds themselves differ; the cells say how, and
[the vocabulary's page](dsl/common-infrastructure-types.md) says why.

The configuration management types have [their own matrix](conformance-matrix.md).

## `network`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| a cidr | yes | ok | ok | ok | ok | ok |
| name and tags | yes | ok | ok | ok | ok | ok |
| a location | no | `unsupported-param` | ok | `unsupported-param` | ok | ok |
| no cidr | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| a param outside the vocabulary (`dns`) | yes | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` | `unsupported-param` |
| `cidr` is a list | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a tag that is not text | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| state `absent` | yes | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` | `unsupported-state` |

## `subnet`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| in a network | yes | ok | ok | ok | ok | ok |
| `public: true` | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` |
| two public subnets in one network | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` |
| a location (availability zone) | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` |
| tags | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` |
| `network` is a name, not a reference | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| `network` refers to nothing | yes | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` | `invalid-reference` |
| no network | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |
| `public` is text | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |

## `firewall`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| inbound rules | yes | ok | ok | ok | ok | ok |
| an outbound rule | yes | ok | ok | ok | ok | ok |
| icmp and any protocol | yes | ok | ok | ok | ok | ok |
| no rules | yes | ok | ok | ok | ok | ok |
| a rule with no cidr | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a port range backwards | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a direction no cloud has (`both`) | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| an icmp rule with a port | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |

## `address`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| no params | yes | ok | ok | ok | ok | ok |
| a name | no | `unsupported-param` | ok | `unsupported-param` | ok | ok |
| tags | yes | ok | ok | ok | ok | ok |

## `vm`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| size and image by name, with a key | yes | ok | ok | ok | ok | ok |
| the cloud's own size and image as text | yes | ok | ok | ok | ok | ok |
| an address and a firewall | yes | ok | ok | ok | ok | ok |
| two firewalls | no | ok | `unsupported-param` | ok | `unsupported-param` | `unsupported-param` |
| no ssh key | no | ok | `missing-param` | ok | `missing-param` | `missing-param` |
| a user | no | `unsupported-param` | ok | `unsupported-param` | ok | ok |
| bigger than any size in the table | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| an image family the vocabulary lacks (`windows`) | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| a size with no memory | yes | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` | `invalid-param` |
| no subnet | yes | `missing-param` | `missing-param` | `missing-param` | `missing-param` | `missing-param` |

## `reads`

| Document | same | terraform (aws) | terraform (azure) | opentofu (aws) | opentofu (azure) | bicep |
|---|---|---|---|---|---|---|
| `publicAddress` of a vm with an address | yes | ok | ok | ok | ok | ok |
| `publicAddress` of a vm with no address | no | ok | `invalid-reference` | ok | `invalid-reference` | `invalid-reference` |
| `privateAddress` and `id` of a vm | yes | ok | ok | ok | ok | ok |
| `address` of an address | yes | ok | ok | ok | ok | ok |
| an id taken by what a common type adds (`vm_nic`) | no | ok | `duplicate-resource` | ok | `duplicate-resource` | `duplicate-resource` |

