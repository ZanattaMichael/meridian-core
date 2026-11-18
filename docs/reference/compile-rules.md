# Compile rules reference

Compile rules are policies over hierarchy layers, checked before compiling. Concepts:
[compile rules](../concepts/compile-rules.md). Examples:
[`examples/compile-rules/`](../../examples/compile-rules/README.md).

## The files

```
config/
├── hierarchy.yaml        the layers, and how each key merges
├── rules.yaml            the compile rules: its own file, never a layer
├── common.yaml           organization
├── environment/
│   └── prod.yaml         environment
├── role/
│   └── web.yaml          team
└── node/
    └── web01.yaml        team
```

`rules.yaml` sits beside `hierarchy.yaml`. A layer path resolving to it is refused.

```mermaid
flowchart LR
    subgraph hierarchy["hierarchy.yaml: least specific first"]
        direction LR
        A["common.yaml"] --> B["environment/prod.yaml"] --> C["role/web.yaml"] --> D["node/web01.yaml"]
    end
    D --> E["--data files"]
    R["rules.yaml"] -. "checks every layer,<br/>is not one" .-> hierarchy
    R -.-> E
    style R fill:#e8f0fe,stroke:#1a56db,color:#000
```

## The hierarchy file

```yaml
hierarchy:
  - common.yaml
  - environment/%{env}.yaml
  - role/%{role}.yaml
  - node/%{node}.yaml

default:
  strategy: deep-merge

keys:
  packages:
    strategy: unique-array-merge

strictness: lenient
```

| Field | Value |
|---|---|
| `hierarchy` | the layer files, **least specific first**, relative to the hierarchy file. A later layer is more specific and wins a merge. `%{name}` is replaced by the variable `name`, given with `--set name=value` on `meridian rules check` |
| `default.strategy` | how a top-level key merges when `keys` does not say: `first-found` (the default), `deep-merge` or `unique-array-merge` |
| `keys.<key>.strategy` | the strategy for one top-level key |
| `keys.<key>.dedupKey` | for `unique-array-merge`: the field that identifies an item that is a mapping |
| `strictness` | `lenient` (the default): a layer file that does not exist is skipped. `strict`: it is an error |

| Strategy | What the more specific layer's value does |
|---|---|
| `first-found` | replaces the less specific layer's value for that top-level key, whole |
| `deep-merge` | mappings merge key by key at every depth; anything else is replaced |
| `unique-array-merge` | lists are joined and repeated items removed |

## The rules file

```yaml
rulesVersion: 1
default: deny

layers:
  - name: organization
    paths: [common.yaml]
    capabilities:
      keys: ["**"]
  - name: environment
    paths: ["environment/*"]
    capabilities:
      keys: [variables, registry]
  - name: team
    paths: ["role/**", "node/**"]
    capabilities:
      keys: [variables, packages, web]

use:
  - protect-variables
  - protect-secrets
  - no-placeholder-secrets

rules:
  - id: region-is-final
    rule: final
    keys: [variables.Region]
```

| Field | Required | Value |
|---|---|---|
| `rulesVersion` | yes | `1` |
| `default` | no | `allow` or `deny`: what a group may do with a [capability](#capabilities) it does not list. Left out: `allow`. Use `deny` |
| `layers` | no | the [groups](#groups): who owns which layer, and what each may set |
| `use` | no | the [shipped rule sets](#shipped-rule-sets) in force. Left out: the three default ones. `use: []`: none |
| `rules` | no | your own [rules](#rules) |

**By default a rules file is a deny list.** `rulesVersion: 1` alone applies the
[default sets](#shipped-rule-sets): no changing a less specific layer's variables, secrets or
credentials, and no empty or placeholder secrets. **Groups with capabilities** say what each
layer may set; **rules** say what a layer may do to another's values.

| You want | Write |
|---|---|
| the default deny list | nothing: leave `use` out |
| the default deny list and rules of your own | `rules:`, and leave `use` out |
| to choose the shipped sets yourself | `use: [protect-secrets]`: exactly the sets listed are in force |
| no shipped set at all | `use: []` |
| an allow list: a layer may set only what its group lists | `default: deny`, and `capabilities` on each group |

With no rules file nothing is checked (`--require-rules` makes that an error). Every rule
has:

| Field | Required | Value |
|---|---|---|
| `id` | yes | a name for the rule, unique in the file. It opens every refusal the rule produces. Ids that start `meridian/` belong to the shipped rules |
| `rule` | yes | the kind of rule: `no-override`, `final`, `restrict`, `required`, `forbidden-value` or `merge-strategy` |
| `keys` | yes | the [key patterns](#key-patterns) the rule is about |
| `message` | no | your own words, added to every refusal the rule produces: who to ask, where the policy is written down |

Unknown fields and undeclared groups are [errors](#mistakes-in-the-rules-file).

## Groups

```yaml
default: deny
layers:
  - name: organization
    paths: [common.yaml]
    capabilities:
      keys: ["**"]
  - name: team
    paths: ["role/**", "node/**"]
    capabilities:
      keys: [variables, packages, web]
```

A group is a named set of layers, used by capabilities and rules.

| Field | Value |
|---|---|
| `name` | the group's name, unique. Names in brackets are Meridian's own |
| `paths` | patterns matched against a layer's path as the hierarchy expands it, such as `node/web01.yaml` |
| `capabilities` | what the group's layers may use; see [capabilities](#capabilities) |

`*` and `?` match within one path element; `**` matches any number. Case-sensitive.

| Pattern | Matches | Does not match |
|---|---|---|
| `common.yaml` | `common.yaml` | `x/common.yaml` |
| `node/*` | `node/web01.yaml` | `node/eu/web01.yaml` |
| `node/**` | `node/web01.yaml`, `node/eu/web01.yaml` | `role/web.yaml` |
| `**/*.yaml` | every `.yaml` layer at any depth | `common.json` |

**The first matching group owns the layer.** **An unclaimed layer may set nothing:**

```
[meridian/unclaimed-layer] layer "extra.yaml" is not claimed by any group under `layers`; a layer nobody owns may set nothing
```

Built-in groups: `(command line)` ([see](#the-command-line-as-a-layer)) and `(unclaimed)`.
Groups are optional.

### Capabilities

What a group's layers may set; `default` decides the rest.

| Capability | Value | Governs |
|---|---|---|
| `keys` | a list of [key patterns](#key-patterns) | the keys a layer of the group may set. An empty list allows none |

| `default` | A group that lists `capabilities.keys` | A group that does not |
|---|---|---|
| `deny` | may set only those keys | may set nothing |
| `allow` (or left out) | may set only those keys | may set anything |

```
[meridian/capabilities] layer "node/rogue.yaml" (team) sets "secrets.database.path", which team is not allowed to set: it may set only variables, packages, web
[meridian/capabilities] layer "Projects/x.yaml" (teams) sets "b", which teams is not allowed to set: it lists no `capabilities.keys`, and the default is deny
```

- Each layer is judged by its own group.
- May set `variables.Owner` does not allow `variables: null`.
- Unclaimed layers may set nothing.
- `default: deny` without groups is refused.
- Capabilities are checked before rules and do not excuse breaking one.
- Capabilities do not apply to the [command line](#the-command-line-as-a-layer); rules do.

`keys` is the only capability today.

## Key patterns

Dotted paths to values in a layer:

| Pattern | Selects |
|---|---|
| `variables` | the key `variables` **and everything beneath it** |
| `variables.**` | the same |
| `variables.Region` | that one key, and everything beneath it if it is a mapping |
| `*.password` | `password` directly beneath any top-level key |
| `**.password` | `password` at any depth, including the top |
| `secrets.*.path` | `path` of every entry under `secrets` |
| `**` | everything |

- `*` is one key, `**` any number.
- A pattern covers everything beneath it.
- Case-sensitive.
- Quote keys with dots, stars or quotes: `'variables."app.kubernetes.io/name"'`.
- A list is one value; [`forbidden-value`](#forbidden-value) checks each item.

Refusals print keys the same way.

## Shipped rule sets

| Name | On by default | Rule id in a refusal | Says |
|---|---|---|---|
| `protect-variables` | yes | `meridian/protect-variables` | a more specific layer may not change anything under `variables` that a less specific layer set |
| `protect-secrets` | yes | `meridian/protect-secrets` | the same for everything under `secrets` and under `credentials` |
| `no-placeholder-secrets` | yes | `meridian/no-placeholder-secrets` | nothing under `secrets` or `credentials` may be empty or a placeholder |
| `protect-all` | no | `meridian/protect-all` | a more specific layer may not change any key a less specific layer set |

**The three defaults apply unless `use` is given.** `use` lists exactly the sets in force;
`use: []` turns them off.

```yaml
use: [protect-variables, protect-secrets, no-placeholder-secrets]   # the same as leaving `use` out
```

The `protect-*` sets are [`no-override`](#no-override) rules over every layer. For narrower
scope, turn them off and use [`setBy` and `appliesTo`](#no-override).

`no-placeholder-secrets` is a [`forbidden-value`](#forbidden-value) rule refusing, under
`secrets` or `credentials`:

| Value | Examples |
|---|---|
| nothing | `null`, `~`, `""` |
| a placeholder, in any case | `changeme`, `change-me`, `change_me`, `replaceme`, `replace-me`, `replace_me`, `placeholder`, `todo`, `tbd`, `fixme` |
| a run of x | `xxx`, `XXXXXXXX` |
| anything in angle brackets | `<your token here>` |

```
[meridian/no-placeholder-secrets] layer "node/web01.yaml" (team) sets "secrets.api.token" to a value the rule forbids (matches[0])
```

Only whole values match (`todo-list-service` is fine).

## Rules

### `no-override`

A more specific layer may not change a value a less specific layer set.

```yaml
- id: org-values-hold
  rule: no-override
  keys: [variables, secrets]
  setBy: [organization]
  appliesTo: [team]
```

| Field | Value |
|---|---|
| `setBy` | the groups whose values are protected. Left out: every layer's |
| `appliesTo` | the groups that are constrained. Left out: every layer |

What counts, for a selected value a less specific layer set:

| The more specific layer | Result |
|---|---|
| sets a different value | refused |
| sets a value of another type (`5432` and `"5432"`) | refused |
| replaces a mapping above it with a scalar, `null` or a list | refused, once for each value beneath |
| sets a sibling under a key that merges by replacing it whole, leaving the value out | refused: the value is dropped |
| replaces a list with another list, under `deep-merge` or `first-found` | refused |
| sets the same value again | allowed |
| adds a key nobody set | allowed |
| adds items to a list that merges with `unique-array-merge` | allowed: nothing is lost |

```
[org-values-hold] layer "node/web01.yaml" (team) changes "variables.Region", which layer "common.yaml" (organization) set
[org-values-hold] layer "node/web01.yaml" (team) sets "variables", replacing "variables.Region", which layer "common.yaml" (organization) set
[org-values-hold] layer "node/web01.yaml" (team) sets "variables", which is merged by replacing it whole (first-found), and so drops "variables.Region", which layer "common.yaml" (organization) set
```

The refusal names the least specific layer that set it.

### `final`

Once a layer sets the key, no more specific layer may set it at all, not even
to the same value. Takes `setBy` and `appliesTo` as `no-override` does.

```yaml
- id: region-is-final
  rule: final
  keys: [variables.Region]
```

```
[region-is-final] layer "node/web01.yaml" (team) sets "variables.Region", which is final once set, and layer "common.yaml" (organization) set it
```

Whichever layer sets it first wins. A final list cannot be added to.

### `restrict`

Only the named groups may set the key.

```yaml
- id: secrets-belong-to-the-organization
  rule: restrict
  keys: [secrets, credentials]
  allow: [organization]
```

| Field | Value |
|---|---|
| `allow` | required: the groups that may set the keys. An empty list means no layer may |

```
[secrets-belong-to-the-organization] layer "node/web01.yaml" (team) sets "secrets.mine.from", which only organization may set
[no-debug] layer "common.yaml" (organization) sets "debug", which no layer may set
```

Replacing a parent (`secrets: null`) counts as setting it:

```
[s] layer "node/x.yaml" (team) sets "secrets", replacing "secrets.db.password.from", which layer "common.yaml" (organization) set and only organization may set
```

It does not stop allowed groups overriding each other; that is
[`no-override`](#no-override).

### `required`

The key must be set. `keys` are exact here: no wildcards.

```yaml
# In the merged result: from whichever layer.
- id: cost-centre-is-set
  rule: required
  keys: [variables.CostCentre]

# In every layer of a group.
- id: team-layers-name-an-owner
  rule: required
  keys: [variables.Owner]
  appliesTo: [team]
```

| Field | Value |
|---|---|
| `appliesTo` | left out: the key must be set in the merged result. Given: every layer of these groups must set it itself |

A key that is `null` is not set.

```
[cost-centre-is-set] no layer sets "variables.CostCentre", which must be set
[team-layers-name-an-owner] layer "node/web01.yaml" (team) does not set "variables.Owner", which every layer of team must set
```

The first form checks the merged result; the second ignores missing layer files.

### `forbidden-value`

The key may not hold one of these values.

```yaml
- id: no-floating-tags
  rule: forbidden-value
  keys: ["**.image"]
  values: [latest]
  matches: ["*:latest"]
  ignoreCase: true
```

| Field | Value |
|---|---|
| `values` | scalars compared exactly, type included: `"0"` does not forbid `0` |
| `matches` | patterns over the value as text, where `*` is any run of characters and `?` any one. A number or a boolean is matched as it is written; `null` matches nothing |
| `ignoreCase` | `true`: compare text without regard to case. Default `false` |
| `appliesTo` | the groups that are checked. Left out: every layer |

Needs `values` or `matches`. List items are checked one by one:

```
[no-floating-tags] layer "node/web01.yaml" (team) sets "app.sidecars[0].image" to a value the rule forbids (matches[0])
```

The refusal names the rule entry, not the value.

### `merge-strategy`

Limits the merge strategy of exact top-level keys.

```yaml
- id: variables-merge-deep
  rule: merge-strategy
  keys: [variables]
  allow: [deep-merge]
```

```
[variables-merge-deep] "variables" is merged with first-found, and may only be merged with deep-merge
```

It names no layer. Under `first-found`, one layer's `variables` replaces them all.

## How a rule reads a merge

For `no-override` and `final`, it depends on the key's strategy:

```mermaid
flowchart TD
    S["a key both layers set"] --> K{"how does the key merge?"}
    K -- "first-found<br/>(replace)" --> RP["compare the two values leaf by leaf"]
    RP --> RP1["same value: restated"]
    RP --> RP2["different value: changed"]
    RP --> RP3["in the less specific layer only: <b>dropped</b>"]
    K -- "deep-merge" --> DP["both mappings: look inside, key by key"]
    DP --> DP1["same value: restated"]
    DP --> DP2["different value, or a mapping replaced<br/>by something else: changed"]
    DP --> DP3["in the less specific layer only: kept"]
    K -- "unique-array-merge<br/>(append)" --> AP["both lists: joined, nothing lost"]
    RP2 --> V{{"no-override: refused"}}
    RP3 --> V
    DP2 --> V
    RP1 --> F{{"final: refused"}}
    DP1 --> F
    AP --> F
    style V fill:#fde8e8,stroke:#c0392b,color:#000
    style F fill:#fde8e8,stroke:#c0392b,color:#000
```

`final` refuses everything `no-override` refuses as well.

## The command line as a layer

Each `--data` file is the most specific layer, `--data <file>`, group `(command line)`,
judged as `first-found`.

```
[meridian/protect-variables] layer "--data override.yaml" ((command line)) changes "variables.OrganizationName", which layer "common.yaml" (organization) set
```

To let the command line set a restricted key, name the group:
`allow: [organization, "(command line)"]`.

## Running the check

```
meridian rules check --hierarchy FILE [--rules FILE] [--require-rules]
                     [--data FILE]... [--set KEY=VALUE]...
```

| Option | Meaning |
|---|---|
| `--hierarchy FILE` | the hierarchy file. Layers are found relative to it |
| `--rules FILE` | the rules file. Default: `rules.yaml` beside the hierarchy file, when there is one |
| `--require-rules` | fail when there is no rules file |
| `--data FILE` | a YAML or JSON file of values, as on `compile`: merged over the hierarchy, each key it names replacing that key whole, and checked as a layer named `--data FILE` in the group `(command line)`; may be repeated |
| `--set KEY=VALUE` | one value, as on `compile`: applied after every `--data` file, `KEY` may be dotted to set one key inside a mapping and keep the keys beside it, and the value is typed as YAML types it. All of them are checked together as one last layer named `--set`, in the group `(command line)`. A layer path's `%{KEY}` reads the same value; may be repeated |

Checks one node's layers without compiling. Run it per node:

```
$ meridian rules check --hierarchy config/hierarchy.yaml --set env=prod --set role=web --set node=web01
rules: config/rules.yaml (9 rules)
layers, least specific first:
  common.yaml            organization
  environment/prod.yaml  environment
  role/web.yaml          team
  node/web01.yaml        team
  --set                  (command line)
no violations
```

It lists each layer's group. `--set` is a layer too, so changing a protected value with it
is refused:

```
$ meridian rules check --hierarchy config/hierarchy.yaml --set env=prod --set role=web --set node=web01 --set variables.Region=us
error: compile rules: 2 violations; nothing was compiled
  - [meridian/protect-variables] layer "--set" ((command line)) changes "variables.Region", which layer "common.yaml" (organization) set
  - [region-is-final] layer "--set" ((command line)) sets "variables.Region", which is final once set, and layer "common.yaml" (organization) set it
```

`--set` changes one key; a `--data` file replaces each top-level key whole.
[Capabilities](#capabilities) apply to neither.

| Exit status | Meaning |
|---|---|
| `0` | every rule holds, or there is no rules file and none was required |
| `1` | a rule is broken, or the hierarchy, a layer or the rules file is wrong |
| `2` | a mistake in the command line (a `--set` without `=` is reported with status `1`, as on `compile`) |

`compile`, `plan` and `apply` do not run rules yet; run `rules check` first
([see](../concepts/compile-rules.md#how-this-fits-the-rest-of-the-language)).

## Refusals

```
$ meridian rules check --hierarchy config/hierarchy.yaml --set env=prod --set role=web --set node=rogue
error: compile rules: 10 violations; nothing was compiled
  - [meridian/capabilities] layer "node/rogue.yaml" (team) sets "credentials", which team is not allowed to set: it may set only variables, packages, web
  - [meridian/capabilities] layer "node/rogue.yaml" (team) sets "secrets.database.path", which team is not allowed to set: it may set only variables, packages, web
  - [meridian/protect-variables] layer "node/rogue.yaml" (team) changes "variables.OrganizationName", which layer "common.yaml" (organization) set
  - [meridian/protect-secrets] layer "node/rogue.yaml" (team) sets "credentials", replacing "credentials.deploy.type", which layer "common.yaml" (organization) set
  - [meridian/protect-secrets] layer "node/rogue.yaml" (team) sets "credentials", replacing "credentials.deploy.user", which layer "common.yaml" (organization) set
  - [meridian/protect-secrets] layer "node/rogue.yaml" (team) changes "secrets.database.path", which layer "common.yaml" (organization) set
  - [meridian/no-placeholder-secrets] layer "node/rogue.yaml" (team) sets "credentials" to a value the rule forbids (values[0])
  - [region-is-final] layer "node/rogue.yaml" (team) sets "variables.Region", which is final once set, and layer "common.yaml" (organization) set it
  - [team-layers-name-an-owner] layer "node/rogue.yaml" (team) does not set "variables.Owner", which every layer of team must set
  - [no-floating-tags] layer "node/rogue.yaml" (team) sets "web.image" to a value the rule forbids (matches[0])
```

Each line: `[rule id]`, layer and group, key, violation, and the rule's `message`. All
violations are listed in a fixed order, no values are printed, and no merged data is
produced.

## Mistakes in the rules file

Reported before any layer is read:

| Message | Cause |
|---|---|
| `rulesVersion: required (write `rulesVersion: 1`)` | the file has no version |
| `rulesVersion: this Meridian reads version 1, found 2` | a file for a newer Meridian |
| `rules file: unknown field "ruls"` | a misspelt top-level field |
| `use[0]: there is no shipped rule set "..."` | a misspelt name under `use` |
| `layers[1].name: the group "a" is declared twice` | two groups with one name |
| `layers[0].paths: the group "a" claims no layers` | a group without `paths` |
| `layers[0].capabilities: unknown field "targets"` | a capability that does not exist |
| `default: expected "allow" or "deny", found "Deny"` | the value is lower case |
| `default: deny needs groups under `layers`: ...` | `default: deny` in a file that declares no groups |
| `rules[0].id: required`, `rules[1].id: the id "a" is used twice` | every rule needs its own id |
| `rules[0].rule: unknown rule "locked"` | not one of the six kinds |
| `rules[0]: a final rule does not take the field "allow"` | a field that belongs to another kind of rule |
| `rules[0].appliesTo: there is no group "team" under `layers`` | a rule names a group the file does not declare |
| `rules[0].keys: the key pattern "a..b" has an empty segment` | a malformed [key pattern](#key-patterns) |
| `rules[0].keys: a required rule names exact keys, and a.* has a wildcard` | `required` and `merge-strategy` take exact keys |
| `rule "m": allow: unknown merge strategy "deep"` | a `merge-strategy` rule names a strategy the hierarchy does not have |
| `the rules file ... is also the hierarchy layer "rules.yaml"` | the hierarchy lists the rules file as a layer |
| `compile rules are required for this run, and there is no rules.yaml ...` | `--require-rules` was given and there is no rules file |

## Protecting the rules file

- Protect `rules.yaml` and `hierarchy.yaml` with CODEOWNERS and branch protection.
- Pass `--require-rules` in pipelines.
- Or keep rules elsewhere and pass `--rules FILE`.
- Put every layer folder in a group.

## From DscPipelineRunner

DscPipelineRunner's
[`Policy.yml`](https://github.com/ZanattaMichael/Dsc.PipelineRunner/blob/main/Example%20Policy%20Configuration/Policy.yml)
maps to `rules.yaml`:

<table>
<tr><th>DscPipelineRunner <code>Policy.yml</code></th><th>Meridian <code>rules.yaml</code></th></tr>
<tr><td>

```yaml
PolicyVersion: 1
Default: Deny
Layers:
  - Name: Platform
    Paths:
      - Datum.yml
      - Platform/**
      - Composites/**
    Capabilities:
      Targets: [ '*' ]
      Engines: [ '*' ]
      ExecutionScripts: true
      DatumScriptBlocks: true
  - Name: Teams
    Paths:
      - Projects/**
    Capabilities:
      Targets: [ Local, WinRM ]
      Engines: [ DscV2, DscV3 ]
```

</td><td>

```yaml
rulesVersion: 1
default: deny
layers:
  - name: platform
    paths:
      - "Platform/**"
    capabilities:
      keys: ["**"]



  - name: teams
    paths:
      - "Projects/**"
    capabilities:
      keys: [variables, packages]


```

</td></tr>
</table>

| `Policy.yml` | `rules.yaml` | Same? |
|---|---|---|
| A file of its own beside `Datum.yml`, never merged | a file of its own beside `hierarchy.yaml`, never merged; a hierarchy that lists it as a layer is refused | yes |
| `PolicyVersion: 1` | `rulesVersion: 1` | yes |
| `Default: Deny` or `Allow`, for a capability a layer does not list | `default: deny` or `allow` | yes, written in lower case. Left out, it is `allow`: a rules file is a deny list unless it says `default: deny` |
| `Layers`, ordered; the first whose `Paths` match a file owns it | `layers`, ordered; the first whose `paths` match owns the layer | yes |
| `Paths` have no file extension, are not case-sensitive, and `*` crosses folders | `paths` include the extension, are case-sensitive, and only `**` crosses folders | no: write `Projects/**`, or `Projects/*.yaml` |
| A file no layer claims may do nothing, whatever `Default` says | the same | yes |
| A layer is decided by where a file is, never by what it says | the same | yes |
| Something several layers contributed to is allowed only what every one of them is allowed | each layer is held to its own group for every key it sets | yes |
| `Capabilities`: `Targets`, `Engines`, `ExecutionScripts`, `DatumScriptBlocks` | `capabilities`: `keys` | the capabilities differ; see below |
| Read strictly: an unknown key is an error | the same | yes |
| Every violation is listed and nothing runs | every violation is listed and nothing is compiled | yes |
| `-PolicyPath`, `-RequirePolicy` | `--rules`, `--require-rules` | yes |
| `<node>.provenance.json` lists each layer and its owner | `meridian rules check` prints each layer and its group | printed, not written to a file |

Differences: Meridian layers hold data, not resources, so capabilities govern keys.
Overriding a platform variable is refused by the default `protect-variables`
([sets](#shipped-rule-sets)).

The hierarchy file:

| DscPipelineRunner `Datum.yml` | Meridian `hierarchy.yaml` |
|---|---|
| `ResolutionPrecedence`, most specific first | `hierarchy`, **least** specific first: reverse the list |
| `Projects\$($Node.Project)` | `Projects/%{Project}.yaml`, with `--set Project=...` |
| `lookup_options: variables: merge_hash_array: deep` | `keys: variables: strategy: deep-merge` |
| `lookup_options: resources: merge_hash_array: UniqueKeyValTuples`, `tuple_keys: [name]` | `keys: resources: strategy: unique-array-merge`, `dedupKey: name`. The first layer to declare a name keeps its entry; entries are not merged |

## Using the rules from Rust

Crate `meridian-rules`:

```rust
let loaded = meridian_rules::enforce(&rules, &hierarchy, &scope, &loader, &command_line)?;
// loaded.resolved.data is the merged data. It exists only if every rule held.
```

| Item | Use |
|---|---|
| `Rules::parse(name, bytes)` | read a rules file |
| `enforce(rules, hierarchy, scope, loader, command_line)` | load, merge, check; `Ok(Loaded)` or `Err(Refusal)` |
| `check_files(files, scope, command_line)` | the same from paths on disk, finding `rules.yaml` and refusing a rules file that is a layer |
| `evaluate(rules, stack)` | check rules over any `Stack`: layers, a `MergePlan` and the merged result |
| `MergePlan` | how a stack merges: `kind(path)` gives `Replace`, `Deep` or `Append`. Implemented for `meridian_resolve::Hierarchy`; implement it to check another way of layering with the same rules |
| `Violation` | `rule`, `check`, `layer`, `group`, `key` and `text`, for a caller that reports in its own format |

`evaluate` works on any ordered stack of keyed layers.
