# Target: `arm`

<img src="../branding/logos/meridian-arm.svg" width="72" alt="Meridian ARM plugin logo">

Compiles an Infrastructure document into an Azure Resource Manager (ARM) JSON template
and deploys it to a resource group or a subscription with the `az` command line.

## What it does and does not support

| | |
|---|---|
| Document kind | `Infrastructure` |
| Category | Infrastructure as code |
| Compiles to | `template.json`, the ARM template, and `deployment.json`, where and how to deploy it |
| Resource types | Azure's own, `Namespace/type`, with the API version in the `apiVersion` param; plus `deployment` for the deployment's settings and `parameter` for a template parameter |
| Scopes | Resource group and subscription. Management group and tenant deployments are not supported |
| References | Yes: a resource's id, name, type, API version and state, and parameters. Checked at compile time |
| ARM expressions | Yes, written in square brackets and passed through after a check that they are well formed |
| `dependsOn` | Yes, as ARM's `dependsOn`. Every reference is written as one too, because Resource Manager infers no order from a resource id |
| `notifies` | No. Resource Manager has no way to react to a change in another resource. Refused |
| `runtimeWhen` | No. An ARM `condition` is decided before anything is deployed. Refused |
| `adopt` | Accepted, and changes nothing: Resource Manager updates a resource of the same name in place. The compile warns |
| `state: absent` | No. A template only says what should exist. Refused; remove the resource, and use mode `Complete` if it should be deleted |
| Plan | Yes: `what-if`. Where Azure cannot analyse a resource, the plan is reported as unknown |
| Apply | Yes: `az deployment group create` or `az deployment sub create` |
| Outputs | Yes, always as strings |
| Template `variables` and `functions` | No. Write the expression where it is used |
| Typed outputs | No. A Meridian output has no type |
| Checking a resource's properties against its type | Not at compile time: the plugin checks the template's structure, references and expressions; Azure checks the properties, and a plan reports what it says. An editor checks them once the type definitions are synced; see [Editor schemas](#editor-schemas) |

![How the ARM plugin compiles and deploys a document](images/arm-flow.svg)

Compiling needs nothing installed and never touches the network. Planning and applying
need the Azure CLI, signed in (`az login`) with rights on the resource group or
subscription the document deploys to.

## A first document

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: logs
spec:
  target: arm
  resources:
    - id: deployment
      type: deployment
      params:
        resourceGroup: logs-rg

    - id: logs
      type: Microsoft.Storage/storageAccounts
      params:
        apiVersion: "2023-01-01"
        name: "[concat('logs', uniqueString(resourceGroup().id))]"
        location: "[resourceGroup().location]"
        kind: StorageV2
        sku:
          name: Standard_LRS

  outputs:
    - name: blob_endpoint
      value: ${logs.properties.primaryEndpoints.blob}
```

It compiles to two files. `template.json` is the template, and `deployment.json` records
where and how to deploy it, for the plugin's own use:

```json
{
  "$schema": "https://schema.management.azure.com/schemas/2019-04-01/deploymentTemplate.json#",
  "contentVersion": "1.0.0.0",
  "metadata": {
    "description": "Compiled by Meridian from the document logs. Do not edit."
  },
  "outputs": {
    "blob_endpoint": {
      "type": "string",
      "value": "[string(reference(resourceId('Microsoft.Storage/storageAccounts', concat('logs', uniqueString(resourceGroup().id))), '2023-01-01').primaryEndpoints.blob)]"
    }
  },
  "resources": [
    {
      "apiVersion": "2023-01-01",
      "kind": "StorageV2",
      "location": "[resourceGroup().location]",
      "name": "[concat('logs', uniqueString(resourceGroup().id))]",
      "sku": {
        "name": "Standard_LRS"
      },
      "type": "Microsoft.Storage/storageAccounts"
    }
  ]
}
```

Keys are written in sorted order at every level, so the same document always compiles to
the same bytes. Resource Manager does not care about key order.

## Resources

A resource's `type` is the ARM resource type, such as `Microsoft.Network/virtualNetworks`,
and its `params` are the resource exactly as the
[Azure template reference](https://learn.microsoft.com/azure/templates/) documents it:
`apiVersion`, `name`, `location`, `properties`, `sku`, `kind`, `tags`, `identity`, `zones`,
`condition`, `copy` and so on. Meridian does not rename or wrap anything.

- `apiVersion` is required, such as `"2023-09-01"`.
- `name` may be left out, and is then the resource's `id`.
- `type` and `dependsOn` are not params. The type is the resource's own `type`, and an
  order is given with the resource's `dependsOn`, as in any Meridian document.
- A child resource has a type with more levels and a name with as many parts:
  `Microsoft.Sql/servers/databases` is named `my-server/orders`.
- An extension resource, such as a lock or a role assignment, names what it is attached to
  with `scope`.

Two resource types are not Azure's:

| Type | What it is |
|---|---|
| `deployment` | The settings of the deployment. At most one per document. It is not written into the template. |
| `parameter` | A template parameter, named by the resource's `id`. |

### The deployment

| Param | Meaning | Default |
|---|---|---|
| `scope` | `resourceGroup` or `subscription` | `resourceGroup` |
| `resourceGroup` | The resource group to deploy to. Required at resource group scope. The group must already exist. | |
| `location` | Where the deployment's record is kept. Required at subscription scope, and not accepted at resource group scope. | |
| `name` | The deployment's name in Azure | the document's `metadata.name` |
| `subscription` | The subscription's name or id | the Azure CLI's current one |
| `mode` | `Incremental`, or `Complete` to delete whatever the group holds that the document does not declare. Resource group scope only. | `Incremental` |

### Parameters

```yaml
- id: admin_password
  type: parameter
  params:
    type: securestring
```

The params are the parameter's declaration: `type` (`string`, `securestring`, `int`,
`bool`, `object`, `secureObject` or `array`) and optionally `defaultValue`,
`allowedValues`, `minValue`, `maxValue`, `minLength`, `maxLength`, `metadata` and
`nullable`. Read it anywhere with `${admin_password}`. How a value is given is under
[Giving parameters their values](#giving-parameters-their-values).

A `securestring` or `secureObject` whose default is written in the document compiles, with
a warning: the secret would be stored in the template and in the deployment's history.

## References

`${...}` wires one resource to another. The plugin turns each reference into the ARM
expression that reads it.

| You write | The template gets | Where |
|---|---|---|
| `${web_vnet}` or `${web_vnet.id}` | `[resourceId('Microsoft.Network/virtualNetworks', 'web-vnet')]` | anywhere |
| `${web_vnet.name}` | `web-vnet` | anywhere |
| `${web_vnet.type}`, `${web_vnet.apiVersion}` | the type and the API version, as text | anywhere |
| `${web_ip.properties.ipAddress}` | `[reference(resourceId(...), '2023-09-01').ipAddress]` | `properties` and outputs |
| `${web_vm.identity.principalId}` | `[reference(resourceId(...), '2023-09-01', 'Full').identity.principalId]` | `properties` and outputs |
| `${admin_password}` | `[parameters('admin_password')]` | anywhere |
| `${names[0]}` | `[parameters('names')[0]]` | anywhere |

What is known when compiling, such as a name written as plain text, is written out
instead of computed. In a deployment to a subscription, `resourceId` is
`subscriptionResourceId`, and a resource with a `scope` is addressed with
`extensionResourceId`.

Reading a resource's state needs ARM's `reference()` function, which Resource Manager
allows only inside a resource's `properties` and in outputs. Anywhere else, such as in
`name` or `tags`, a state reference is refused at compile time with the reason, and so is
a resource reading its own state.

### References inside text

A reference inside longer text becomes one `format()`:

```yaml
subnet:
  id: ${web_vnet}/subnets/web
# "[format('{0}/subnets/web', resourceId('Microsoft.Network/virtualNetworks', 'web-vnet'))]"
```

Write `$${` for a literal `${`.

### ARM expressions

A string in square brackets is an ARM expression. It is passed through as written, after a
check that it is well formed, so a missing parenthesis is reported against your document
and not by a failed deployment:

```yaml
location: "[resourceGroup().location]"
```

A reference inside an expression is put in place as an argument:

```yaml
name: "[concat('weblogs', uniqueString(resourceGroup().id, ${web_vnet.name}))]"
# "[concat('weblogs', uniqueString(resourceGroup().id, 'web-vnet'))]"
keys: "[listKeys(${logs}, ${logs.apiVersion}).keys[0].value]"
```

To write literal text that starts with `[` and ends with `]`, start it with `[[`, as in
any ARM template. The plugin reads `[[` at the start of any string as a literal `[`, and
writes the doubled bracket back only where Resource Manager would otherwise read an
expression, so `"[[not closed"` is deployed as `[not closed`.

A value Meridian places from `--data`, `--set`, a data layer or `env()` is never an
expression, whatever it holds: the plugin declares the opening `[`, and the core hands a
placed value that starts with one over as `[[`. A data value of `[listKeys(...)]` is
deployed as that text, and reads nothing from the subscription
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).
A reference the author wrote inside an expression is an argument, as above; data placed
inside an expression the author wrote is placed as text within it.

## Ordering

Resource Manager deploys resources in parallel and does not infer an order from a
resource id. So the plugin writes a `dependsOn` entry for every resource another one
refers to, and for every `dependsOn` in the document. You never write ARM's `dependsOn`
yourself.

```yaml
- id: web_vm
  type: Microsoft.Compute/virtualMachines
  params: { ... }
  dependsOn:
    - resource: web_nsg   # nothing in the params mentions it
```

Resources that depend on each other in a circle are refused at compile time, with the
circle spelled out: `a -> b -> c -> a`.

## Outputs

Each output of the document becomes a template output, under the same name, and is
reported back when the document is applied.

```yaml
outputs:
  - name: web_vm_ip
    value: ${web_ip.properties.ipAddress}
  - name: web_url
    value: http://${web_ip.properties.dnsSettings.fqdn}/
```

A Meridian output has no declared type, and an ARM output must have one, so every output
is declared as a `string`. A value that might not be text is converted with `string()`: a
number arrives as its digits, and an object or an array as JSON text.

## `notifies`, `runtimeWhen` and `state`

None of the three has anything in a template to compile into, so each is refused when
the document is compiled, with the reason:

- `notifies` on any resource, whatever the action: `unsupported-action`.
- `runtimeWhen`: `unsupported-runtime-condition`. ARM's own `condition` is a param like
  any other and is passed through; it is decided from the template's parameters before
  anything is deployed.
- `state: absent`, or any state other than `present`: `unsupported-state`.

## Plan, apply and outputs

| Operation | What the plugin runs |
|---|---|
| Plan | `az deployment group what-if --name <name> --resource-group <rg> --template-file template.json --mode <mode> --no-pretty-print` |
| Apply | the same preview, so the changes can be reported, then `az deployment group create` with the same arguments |
| Outputs later | `az deployment group show --name <name> --resource-group <rg>` |

At subscription scope the commands are `az deployment sub ...`, with `--location` in
place of `--resource-group` and `--mode`. `--subscription` is added when the deployment
names one. Every call asks for `--output json`.

How Azure's preview is reported:

| Azure says | Meridian reports |
|---|---|
| `Create` | create |
| `Delete` | delete |
| `Modify` | update, listing up to five of the properties that change |
| `Deploy` | update, saying that the resource is deployed again and the preview cannot tell what changes |
| `NoChange`, `Ignore` | nothing. `Ignore` is a resource that exists, is not in the template, and is left alone |
| `Unsupported` | nothing, and the plan is marked unknown, so it is never reported as in sync |

Changes are reported under the ids you wrote in the document. A resource whose name is
only known when deploying, and one the document does not declare, are reported under
their Azure resource id.

An apply always creates the deployment, even when the preview shows nothing to change, so
that its record and outputs exist. If the preview fails or Resource Manager rejects the
template, nothing is deployed. A failed deployment is reported with the end of what `az`
printed; Azure does not roll back a failed deployment.

Outputs are read from the deployment's record. An output of a secure type is reported as
sensitive, and Azure does not return its value. Asking for outputs before the document
was ever applied is an error that says so.

Credentials are the `az` command's own (`az login`). Meridian stores and passes none. The
target does not yet take a [cloud credential](../reference/dsl/credentials.md#reaching-a-cloud-as-a-credential)
from a document: `az login --service-principal` takes a secret on its command line only,
which Meridian never does, so a document that names one for `arm` is refused before
anything runs. Declare the credential `from: ambient` to name the identity the signed-in
`az` is.

Every template this plugin writes is read by the real Bicep compiler's ARM reader
(`az bicep decompile`) on every pull request that touches it, which has to parse the whole
template. The runner itself (`az deployment group what-if`, `create` and `show`) has only
met a stand-in for the `az` command: Azure has no emulator, so running it for real needs a
subscription. The job that does so is written and is switched on by four repository
settings, listed under [Real-tool tests](../contributing/real-tool-tests.md#azure). Until it has run, treat the runner as
untested against Azure.

### Environment

| Variable | Meaning |
|---|---|
| `MERIDIAN_AZ` | The program to run, when it is not `az` on `PATH` |
| `MERIDIAN_AZ_ARGS` | Extra arguments for every call, split like a shell command line (quotes hold a word together), such as `--only-show-errors` |
| `MERIDIAN_AZ_PARAMETERS` | Values for the template's parameters, split like a shell command line (quotes hold a word together) |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.arm`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#arm).

### Giving parameters their values

There are two ways, and they can be combined.

- **From the operator.** Each word of `MERIDIAN_AZ_PARAMETERS` is passed to
  `--parameters`, so it takes whatever `az` takes: a parameters file or a pair.

  ```sh
  export MERIDIAN_AZ_PARAMETERS="@prod.parameters.json admin_user=ops"
  ```

- **From another document of the workflow.** A value the document
  [imports](../reference/dsl/outputs-and-hand-offs.md#imports) under the name of one of
  the template's parameters is that parameter's value. This is how a value made by
  another tool, or a file another document compiles to, reaches a template.

Operator values are never shown in a log or an error, and neither is the value of a
`securestring` or `secureObject` parameter.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in a resource's params
reaches Resource Manager as a `securestring` parameter whose value is read from a private
file, and through nothing else.

```yaml
spec:
  target: arm
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_password_secret
      type: Microsoft.KeyVault/vaults/secrets
      params:
        apiVersion: "2023-07-01"
        name: app-vault/db-password
        properties:
          value: ${{ secret.db_password }}
```

**What the compile writes.** The param becomes
`[parameters('meridian_secret_db_password')]`, or a `format()` of it inside longer text,
and the template declares the parameter beside the document's own:

```json
"parameters": {
  "meridian_secret_db_password": { "type": "securestring" }
}
```

The compiled template holds the parameter's name and never its value. `deployment.json`
records the parameter as secure, so the runner never shows a value given under that
name.

**How the value reaches the tool.** When a stage runs, the runner writes every secret into
one Resource Manager parameters file (`"parameters": {"<name>": {"value": ...}}`) in a
directory only the user can enter (mode `0700`, the file `0600`, on a memory filesystem
where the system has one), and passes it as `--parameters @<path>` to both `what-if` and
`create`, before any value from `MERIDIAN_AZ_PARAMETERS` or an earlier stage. The file is
overwritten and removed when the stage ends. The command line names the file's path and
nothing more, and `az`'s environment holds nothing of the value.

**What Azure keeps.** A `securestring` parameter's value is not in the deployment's
record and not returned by `show`. A property given the value is kept by the resource
provider the way it keeps that property. The plugin reads each change's resource id,
kind and the names of up to five changed properties from the preview, never a value.
Whether what-if prints a secure value inside a property diff has not been checked
against a real subscription; see the note under [Plan, apply and
outputs](#plan-apply-and-outputs).

**What is refused.** A secret in the `deployment` resource's settings, or in a
`parameter`'s declaration (`defaultValue`, `allowedValues`), is refused at compile, rule
`unsupported-secret-place`: the settings are the command line `az` is given, and a
declaration is written into the template as it stands.

The handover is tested against a stand-in `az` that keeps a copy of the parameters file,
its mode, its arguments and its environment (`plugins/arm/tests/run.rs`), and the
compiled template is read by the real Bicep reader in CI (`plugins/arm/tests/real.rs`),
which reads the parameter back as `@secure()`.

## What is refused

The plugin refuses at compile time what it could not compile faithfully. Each refusal
names the resource, the line it was written on and a rule:

| Rule | Why |
|---|---|
| `unsupported-resource-type` | The type is not `Namespace/type`, `deployment` or `parameter` |
| `missing-param`, `invalid-param`, `unsupported-param`, `reserved-name` | A param is missing, has the wrong form, is unknown, or is one Meridian writes itself |
| `invalid-name` | A name does not have as many parts as its type has levels |
| `invalid-reference` | A `${...}` names nothing in the document, is not a path, reads a resource's state where ARM does not allow `reference()`, or reads the resource's own state |
| `dependency-cycle` | Names are defined by each other, or resources depend on each other in a circle |
| `invalid-expression` | A string in square brackets is not a well-formed ARM expression |
| `duplicate-resource`, `duplicate-output` | The same thing is declared twice. Azure compares names without regard to case. Two resources with the same name are allowed when one has a `condition` or a `copy`. |
| `invalid-name`, `empty-document`, `template-too-large` | Resource Manager's own limits: a deployment name of up to 64 characters, at least one resource, and at most 800 resources, 256 parameters and 64 outputs |
| `unsupported-dependency` | A `dependsOn` names a `parameter` or the `deployment`, which are not resources of the template |
| `unsupported-action` | `notifies` has nothing to compile into: Resource Manager cannot act on one resource because another changed |
| `unsupported-state` | `state: absent`: a template only says what should exist. Remove the resource, and use mode `Complete` if it should be deleted. |
| `unsupported-runtime-condition` | `runtimeWhen`: an ARM `condition` is decided before anything is deployed |

## Editor schemas

`meridian schema sync` reads the same Azure type definitions as the
[Bicep target](bicep.md#editor-schemas), published in
[Azure/bicep-types-az](https://github.com/Azure/bicep-types-az), so an
editor completes and checks a resource's properties
([Tool schemas](../tooling/schema-sync.md)). The difference is how a
document writes a type: `Namespace/type` with the API version in the
`apiVersion` param.

| | |
|---|---|
| Described in full | the `Namespace/type` types the documents use, at the API version the resource's `apiVersion` names, or the newest stable one when it names none: every property as for Bicep, plus `apiVersion` itself, required, with every API version the index has as its values |
| Listed by name | every other type in the index, once, with its `apiVersion` values |
| Pinned | the index and each `types.json` read, to their SHA-256 |

A document whose resources of one type use two API versions is described at
the first; the properties rarely differ between neighbouring versions, and
Azure is the judge of what deploys. The source is tested against the real
files in the `azure` workflow.

## Not covered yet

- Deployments to a management group or a tenant.
- Template `variables` and user-defined `functions`. An expression can be written where it
  is used instead.
- Typed outputs: every output is a string.
- Checking a resource's properties against its type's schema at compile time. The
  plugin checks the template's structure and its expressions; Azure checks the
  properties, and a plan reports what it says. The editor checks them once the
  schemas are synced ([Editor schemas](#editor-schemas)).

## Examples

| Document | What it shows |
|---|---|
| [`examples/arm/web-vm.yaml`](../../examples/arm/web-vm.yaml) | A virtual machine with its network, a secret parameter, every kind of reference |
| [`examples/arm/sql-database.yaml`](../../examples/arm/sql-database.yaml) | Child resources, names made from a parameter, an extension resource, mode `Complete` |
| [`examples/arm/landing-zone.yaml`](../../examples/arm/landing-zone.yaml) | A deployment to a subscription |

What each compiles to is kept beside the plugin's tests, in
[`plugins/arm/testdata`](../../plugins/arm/testdata).

## ARM and Bicep

Bicep is a language that compiles to ARM templates, and Meridian has a separate plugin for
it. Choose `arm` when the JSON template itself is what you need: for a pipeline that takes
templates, a template spec, or a team that reviews the JSON.
