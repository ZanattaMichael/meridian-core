# Refusal rules

Every rule a compile-time refusal can carry, with what it refuses and what to
write instead. This page is generated: `crates/meridian-cli/tests/refusal_rules.rs`
writes it from the registry in `meridian_sdk::rules` and from what each plugin
declares, and fails when it is out of date, so a rule cannot appear in the code
without appearing here.

A refusal is a target saying no to a document at compile time: a resource type
it does not map, a param it does not take, a secret where it cannot keep one.
It is not an [error](errors.md). Each refusal names the target, the resource
and the line it is about, carries a message that says what was wrong and what
that target accepts, and carries a *rule*: one of the names below. The name is
the same for the same mistake on every target, and it does not change once it
is published, so a policy check, an editor, a test or a script can key off it.
Beside the rule travels a *hint*, what to write instead in general terms; the
message is the specific one.

`meridian` prints the rule in square brackets after the resource, and the hint
in parentheses after the message:

```
site.yaml:12:5: ansible: resource "app" [unsupported-param]: param "provider" ... (hint: remove the param, or use the name the message lists)
```

## General rules

The same mistake has the same name on every target, however the target finds
it. A plugin uses one of these wherever it fits.

| Rule | What it refuses | What to write instead |
|---|---|---|
| `unsupported-resource-type` | the resource's `type` is not one this target maps | use a type from the target's resource types; `meridian plugins` lists them |
| `unsupported-state` | the resource's `state` is not one this target can express for its type | use a state the message lists; the target's page says what each does |
| `unsupported-param` | a param this target does not take for the resource's type | remove the param, or use the name the message lists |
| `missing-param` | a param the resource's type needs is not given | add the param the message names |
| `empty-param` | a param is given with no value, where the tool needs one | give the param a value, or remove it |
| `invalid-param` | a param's value is not the kind or form the resource's type takes | write the value as the message says; quote a mode such as "0644" |
| `param-conflicts-with-state` | a param that means nothing in the resource's `state`, such as `content` on a directory | remove the param, or change the state |
| `invalid-name` | the resource's id, or a name it gives, cannot be a name in the tool | use a name of the form the message gives |
| `duplicate-resource` | two resources would be one thing to the tool: the same name, path or title, or a second resource of a type a document has one of | keep one of the two, or give each its own name |
| `invalid-reference` | a `${...}` reference names a resource or value the document does not declare, reads what cannot be read where it stands, or is not well formed | reference a resource of this document and a value it declares; see the references page |
| `unknown-dependency` | `dependsOn` names a resource the document does not declare | name a resource of this document |
| `unsupported-dependency` | an ordering between two resources that this target cannot express | remove the `dependsOn`, or order the resources as the message says |
| `dependency-cycle` | the resources' dependencies form a cycle | remove one `dependsOn` from the cycle |
| `ordering-violated` | the apply order the core produced puts a resource before its dependency, which this target cannot keep | this is a defect in Meridian, not in the document: report it |
| `unknown-notify-target` | a `notifies` names a resource the document does not declare, at either end | name a resource of this document |
| `unsupported-notify` | the target has no way to react to a change, so `notifies` cannot be expressed | remove the `notifies`; see the capability matrix |
| `unsupported-action` | a `notifies` action this target cannot run | use an action the message lists |
| `unsupported-runtime-condition` | a `runtimeWhen` on a target whose tool checks no condition on the node | use a compile-time `when`, or remove the condition; see the capability matrix |
| `invalid-runtime-condition` | a `runtimeWhen` this target cannot turn into its tool's own test | write the condition as the target's page describes |
| `duplicate-output` | two outputs of the document have one name | give each output its own name |
| `unsupported-output` | an output this target cannot report, or a value it cannot read into one | remove the output, or read a value the message says the tool reports |
| `unsupported-secret-place` | a secret in a param this target would write into a file or a command line as given | put the secret where the target's page says it stays secret |
| `unsupported-credential` | the credential `targetHost.credential` names is of a kind this target cannot reach its host with | use a credential kind the target's page lists |
| `unsupported-setting` | a `spec.settings` key this target does not read | remove the key, or use one the message lists |
| `invalid-setting` | a `spec.settings` value that is not what the key takes | write the value as the target's page describes |
| `conflicting-settings` | one setting is given in two places with different values, such as `spec.settings` and `spec.targetVersion` | say it once |
| `reserved-name` | a name, key or text that Meridian itself uses on this target, such as the prefix `meridian_secret_` | choose another name |
| `invalid-host` | `targetHost.host` holds characters the target cannot write into its tool's files | give a host name or address, with nothing but letters, digits, dots, hyphens and colons |
| `missing-host` | the document names no `targetHost`, and this target cannot compile without one | set `spec.targetHost.host` |
| `empty-document` | the document holds nothing the tool would act on, so there is nothing to compile | add a resource |
| `missing-resource` | a resource this target needs in every document is not there, such as a `deployment` | add the resource the message names; the target's page says what it takes |
| `invalid-path` | a path this target cannot use: relative where an absolute one is needed, leaving the directory it must stay in, or holding characters the tool cannot take | write the path as the message says |

## Rules of one target

A mistake that exists on one tool only has a rule of that target's own, which
the plugin declares in one table and `meridian plugins --rules` prints. A
target that is not listed here refuses only under the general rules.

### [arm](../targets/arm.md)

| Rule | What it refuses | What to write instead |
|---|---|---|
| `invalid-expression` | a value written as an ARM template expression, `[...]`, that Resource Manager could not parse | write a well-formed expression, or escape a literal bracket as `[[` |
| `template-too-large` | the compiled template is larger than the 4 MB Resource Manager accepts | split the document into several deployments |

### [cloud-init](../targets/cloud-init.md)

| Rule | What it refuses | What to write instead |
|---|---|---|
| `unsatisfiable-order` | `dependsOn` asks for an order cloud-init cannot keep: the order of a first boot is cloud-init's own, and a document cannot change it | remove the `dependsOn`, or move the work to a resource cloud-init runs later |

### [puppet](../targets/puppet.md)

| Rule | What it refuses | What to write instead |
|---|---|---|
| `unnamed-secret` | a param holds a secret the resource tree does not name, so the manifest cannot refer to it | this is a defect in the host that sent the tree, not in the document: report it |

### [salt](../targets/salt.md)

| Rule | What it refuses | What to write instead |
|---|---|---|
| `shell-required` | an `exec` command holds shell syntax, and Salt always runs a command through a shell | set param `shell` to true to say the shell syntax is intended, or quote the command |

