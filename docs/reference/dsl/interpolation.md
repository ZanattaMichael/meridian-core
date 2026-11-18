# Interpolation, variables and named conditions

An expression can be written inside a value, between `${{` and `}}`. Meridian
decides it before compiling and puts the result in its place, so a plugin is
handed plain values and no target ever sees an expression. That is what makes
an interpolation come to the same value on every target.

```yaml
spec:
  target: ansible
  variables:
    name: ${{ data.project }}-${{ data.env }}
  conditions:
    is_prod: data.env == 'prod'
  resources:
    - id: site_conf
      type: file
      params:
        path: /etc/nginx/conf.d/${{ var.name }}.conf
        workers: "${{ cond.is_prod ? 8 : 1 }}"
```

The language inside the braces is the one a `when` is written in:
[expressions and functions](expressions.md).

```mermaid
flowchart LR
    A["1. data<br/>--data, --set"] --> B["2. variables and<br/>named conditions"]
    B --> C["3. spec.when<br/>document kept or skipped"]
    C --> D["4. when on resources,<br/>edges and outputs<br/>ifMissing applied"]
    D --> E["5. every interpolation<br/>placed"]
    E --> F["6. sorted by dependsOn"]
    F --> G["7. the plugin compiles<br/>references rewritten"]
    style G fill:#eee,stroke:#999
```

Steps 1 to 6 are Meridian's core and are the same for every target. Step 7 is
the only one a plugin takes part in.

## Contents

- [Interpolation](#interpolation)
- [Where it is allowed](#where-it-is-allowed)
- [Beside a reference](#beside-a-reference)
- [Values from data are literal](#values-from-data-are-literal)
- [Writing one in YAML](#writing-one-in-yaml)
- [Variables](#variables)
- [Named conditions](#named-conditions)
- [What is checked, and when](#what-is-checked-and-when)
- [Examples](#examples)

## Interpolation

| You write | You get |
|---|---|
| a value that is only `${{ expr }}` | the expression's value with its type: a number stays a number, a list stays a list, a mapping stays a mapping |
| `${{ expr }}` inside longer text | the value placed in the text. It must be text, a number or a boolean |
| `$${{` | a literal `${{`, by the same rule that makes `$${` a literal `${` |
| `${id.path}` | unchanged: a [reference](references.md), rewritten by the plugin and resolved by the tool |

```yaml
params:
  count: ${{ data.replicas }}                 # 3, a number
  ports: ${{ data.ports }}                    # [80, 443], a list
  tags: ${{ var.tags }}                       # a mapping, with its shape
  name: ${{ data.project }}-${{ data.env }}   # "shop-prod", text
  listen: listen ${{ join(data.ports, ' ') }} # "listen 80 443"
```

Only a value that is nothing but the interpolation keeps its type. A space
before or after it makes it text: `" ${{ data.replicas }}"` is `" 3"`.

A list, a mapping or `null` has no single text form, so placing one inside
longer text is an error that says what to do instead:

```
in ${{ data.ports }}: a list cannot be placed inside text; use join(...) or toJson(...), or make the interpolation the whole value
in ${{ data.nothing }}: the value is null, which has no text form; give a default with ??
```

The end of an interpolation is found past quotes and brackets, so a mapping
literal or a quoted `}}` inside it does not end it early:
`${{ {a: {b: 1}}.a.b }}` is `1`, and `${{ '}}' }}` is the text `}}`.

Everything that is not an interpolation is passed through byte for byte:
`{{ ansible_hostname }}`, `%{ if x }`, `$5` and `${web.public_ip}` all reach the
plugin exactly as written.

## Where it is allowed

| Place | Allowed |
|---|---|
| any value inside `params`, however deep | yes |
| a resource's `state` | yes; it must come to text |
| an output's `value` | yes; it must come to text |
| a variable's value | yes |
| `metadata.labels` values | yes; a [label selector](index.md#labels-and-selectors) matches the value it comes to |
| `spec.target`, `spec.targetVersion` | yes; they must come to text |
| `spec.targetHost.host` | yes |
| `when`, on anything, and a named condition | these are already expressions and are written bare. Wrapped in `${{ }}` they mean the same |
| `apiVersion`, `kind`, `metadata.name`, a label's name, a resource's `id` and `type`, the `resource` of a `dependsOn` or `notifies` entry, a notify's `action`, an output's `name`, `targetHost.from`, `.ref` and `.output`, a param's name and any mapping key | no. These are the document's skeleton, and `validate` must be able to check them without any data |
| `runtimeWhen` | no. It is passed to the tool untouched |

An interpolation in a place that does not take one is refused by `validate`:

```
site.yaml:12:7: spec.resources[1].id: cannot be interpolated: it is part of the document's skeleton, which must be checkable without any data
```

A field that is text refuses a value that is not:

```
site.yaml:9:7: spec.resources[0].state: resource "pkg": this field is text, and the interpolation gives a list
```

## Beside a reference

Two things are written with a dollar sign, and they are decided at different
times.

| | `${{ ... }}` | `${id.path}` |
|---|---|---|
| Decided by | Meridian's core | the tool, through the plugin |
| Decided when | before compiling | while the tool plans or applies |
| Reads | data, variables, named conditions | attributes of resources the tool creates in this run |
| Same on every target | yes | no: it is the tool's own value |

They can share a value:

```yaml
- id: public
  type: aws_subnet
  params:
    vpc_id: ${main.id}                       # resolved by Terraform
    cidr_block: ${{ var.public_cidr }}       # decided by Meridian
    tags:
      Name: ${{ var.name }}-${main.id}       # both
```

A variable is worked out before the tool runs, so it cannot hold a reference:

```
site.yaml:1:1: spec.variables.addr: a variable is worked out before the tool runs, so it cannot hold a ${id.path} reference; write the reference where it is used
```

## Values from data are literal

A value an interpolation places into `params` or an output's `value` is always
literal text to the plugin. If the data holds `${HOME}/bin`, or something that
looks like a reference to another resource, it is handed over with the escape
for a literal `${`, and every plugin writes it out as the text it is.

What the author wrote in the document is a reference. What the data supplied
never becomes one. That holds however the value got there: through a variable,
inside a list or a mapping, or joined into longer text.

| The document says | The data says | The plugin is given | Ansible writes |
|---|---|---|---|
| `cd ${{ data.dir }}` | `dir: ${HOME}/bin` | `cd $${HOME}/bin` | `cd ${HOME}/bin` |
| `cd ${app.dir}` | | `cd ${app.dir}` | the `dir` param of resource `app` |
| `${{ data.sign }}{app.dir}` | `sign: $` | `$${app.dir}` | `${app.dir}`, as text |

The last row is a value that holds no reference by itself but would complete
one with the text beside it: a placed value that ends in `$`, followed by text
or another placed value that opens with `{`. The join is made literal too, so
two harmless halves cannot meet as a reference nobody wrote.

One join cannot be made safe, and is refused. A dollar directly before a
reference is how a literal is written, so a placed value that ends in `$`,
directly before a reference the author wrote, would turn that reference into
text and the tool would never resolve it:

```yaml
content: costs ${{ data.currency }}${price.amount}   # currency: $
```

```
the value placed before ${price.amount} ends in "$", and a "$" directly before a reference makes it literal text, so the reference would be lost; put a space or other text between the two
```

Nothing is compiled. With a space between the two, or with a currency that
does not end in a dollar, both are kept.

Each tool gets its own spelling of the literal: Terraform, for instance, is
given `$${HOME}`, which is how Terraform writes a literal `${HOME}`.

### A tool's own expressions

Some tools read more than `${` as an expression. Ansible renders every task
string through Jinja, so `{{ ... }}` is live. Resource Manager evaluates an ARM
string that starts with `[` and ends with `]`.

- What the author writes in that syntax is passed through, as each target's
  page says.
- What data supplies is not. Each plugin declares the openings its tool reads,
  and Meridian writes a placed value so the tool reads it as text.

| The document says | The data says | Ansible is given | Ansible writes to the file |
|---|---|---|---|
| `${{ data.motd }}` | `motd: hi {{ lookup('pipe', 'id') }}` | `hi {{ '{{' }} lookup('pipe', 'id') }}` | `hi {{ lookup('pipe', 'id') }}`, as text |
| `{{ inventory_hostname }}` | | `{{ inventory_hostname }}` | the host's name |

| The document says | The data says | ARM is given | Azure deploys |
|---|---|---|---|
| `${{ data.loc }}` | `loc: "[resourceGroup().location]"` | `[[resourceGroup().location]` | the text `[resourceGroup().location]` |
| `"[resourceGroup().location]"` | | `[resourceGroup().location]` | the resource group's location |

A join is made literal here too: a placed `{` directly before a `{` the
author wrote cannot open a Jinja expression nobody wrote. Which openings a
target declares is on its page; a target that declares none has no expression
syntax of its own beyond `${`.

## Writing one in YAML

A value that starts with `${{` needs no quotes in YAML as long as the
expression holds nothing YAML reads as its own. Two things do: a colon followed
by a space, which is in every `a ? b : c`, and a space followed by `#`.

```yaml
count: ${{ data.replicas ?? 1 }}                  # fine
name: ${{ data.project }}-${{ data.env }}         # fine
size: "${{ cond.is_prod ? 'large' : 'small' }}"   # needs quotes: it holds ": "
```

Left unquoted, the third line is a YAML error, and Meridian adds the way out:

```
site.yaml:14:36: mapping values are not allowed in this context; this line holds an interpolation, and YAML reads a colon followed by a space inside an unquoted value as its own: put the whole value in double quotes
```

When in doubt, quote the whole value with double quotes and use single quotes
for text inside the expression.

## Variables

`spec.variables` names values that are worked out once per document and read
as `var.<name>`.

```yaml
spec:
  variables:
    name: ${{ data.project }}-${{ data.env }}
    root: /srv/${{ var.name }}
    ports: "${{ cond.is_prod ? [80, 443] : [8080] }}"
    tags:
      Project: ${{ data.project }}
      Owner: ${{ upper(data.team) }}
    workers:
      value: "${{ cond.is_prod ? data.cpus * 2 : 1 }}"
      description: two workers per CPU in production, one anywhere else
```

| Rule | |
|---|---|
| Name | letters, digits and underscores, not starting with a digit, so that it can be written after `var.` |
| Value | anything: text, a number, a boolean, a list, a mapping. Text inside it is interpolated, however deep |
| The long form | a mapping with a `value` key, and optionally a `description`. A mapping without a `value` key is itself the value |
| Order | variables may be written in any order. One may read another, and may read a named condition |
| A circle | variables and conditions that read each other in a circle are refused by `validate`, naming the circle: `var.a -> cond.x -> var.a` |
| Worked out | every variable, once, before anything else in the document is decided. One that cannot be worked out is an error even if nothing reads it |
| Not from outside | a variable is not set from the command line. What the caller chooses comes in as data |

`type`, `enum`, `final` and the other checks of the long form arrive with typed
declarations; until then they are refused by name and not ignored.

A variable holds a plain value. Its type goes with it: `${{ var.ports }}` is a
list, `${{ var.ports[0] }}` is a number, `${{ var.tags.Owner }}` is text.

## Named conditions

`spec.conditions` names conditions that are decided once per document and read
as `cond.<name>`, which is `true` or `false`.

```yaml
spec:
  conditions:
    is_prod: data.env == 'prod'
    needs_tls: cond.is_prod || (data.force_tls ?? false)
  resources:
    - id: site_cert
      type: file
      when: cond.needs_tls
      params: {path: /etc/ssl/site.pem}
    - id: debug_page
      type: file
      when: "!cond.is_prod"
      params: {path: /srv/site/debug.html}
```

| Rule | |
|---|---|
| Name | letters, digits and underscores, not starting with a digit |
| Value | one [expression](expressions.md), written bare, that comes out `true` or `false`. Anything else is an error, never a guess |
| Reads | data, variables and other named conditions, in any order; a circle is refused |
| Used in | any `when`, and any interpolation: `"${{ cond.is_prod ? 3 : 1 }}"` |

A named condition gives a decision one name and one place. The condition
language, and what a false `when` does to a resource, a document, an output or
an edge, are on the [conditions](conditions.md) page.

## What is checked, and when

`meridian validate` needs no data, and finds:

| Problem | Message |
|---|---|
| An interpolation that is never closed | `"${{ data.env" opens an interpolation that is never closed with }}` |
| An empty one | `"x-${{ }}" holds an empty interpolation` |
| An expression that does not read | `in ${{ data.env == }}: the expression ends where a value was expected` |
| A name this place cannot read | `in ${{ endpoint.db }}: unknown name "endpoint" (...)` |
| An unknown function, or the wrong number of arguments | `in ${{ uper(data.a) }}: unknown function "uper"` |
| A variable or condition that is not declared | `var.nmae is not declared (spec.variables declares: name, ports)` |
| A circle | `these read each other in a circle, so none can be worked out: var.a -> var.b -> var.a` |
| An interpolation in the skeleton | `spec.resources[1].id: cannot be interpolated: ...` |
| A reference inside a variable | `a variable is worked out before the tool runs, so it cannot hold a ${id.path} reference; ...` |

`compile`, `plan` and `apply` have the data, and find the rest. Each names the
resource, the field and the interpolation:

```
site.yaml:21:7: spec.resources[1].params.tags.Name[1]: resource "web": in ${{ data.missing }}: data.missing is not set (give it with --data or --set, or test for it with defined(data.missing))
```

Nothing is guessed: a name that is not set is an error, not an empty string.
A resource its `when` removed is not interpolated, and a document its
`spec.when` skipped is not read further, so what only one environment needs is
not an error in another.

## Examples

Under [`examples/language/`](../../../examples/language), each with the data
file it reads beside it:

| Example | Shows |
|---|---|
| [`site-by-environment.yaml`](../../../examples/language/site-by-environment.yaml) | variables, named conditions, an interpolated label and host, a `when` on an edge, on Ansible |
| [`network-by-environment.yaml`](../../../examples/language/network-by-environment.yaml) | interpolation beside references, a whole mapping placed with its shape, a `when` on an output, on Terraform |
| [`optional-monitoring.yaml`](../../../examples/language/optional-monitoring.yaml) | a `spec.when` that leaves a whole document out of the run, on Kubernetes |

All of it together in one workflow of four documents and four tools is
[`examples/workflow/tenants.yaml`](../../../examples/workflow/tenants.yaml),
walked through in [worked workflows](../../quickstart/workflows.md#tenants).

```
meridian compile examples/language/site-by-environment.yaml \
  --data examples/language/site-by-environment.data.yaml --set env=dev
```
