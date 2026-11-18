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

Compiling needs nothing installed and no network. Plan and apply need the Azure CLI,
signed in (`az login`) with rights on the target scope.

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

It compiles to `template.json` and `deployment.json` (where and how to deploy):

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

Keys are sorted.

## Resources

`type` is the ARM type (`Microsoft.Network/virtualNetworks`); `params` are the resource as
the [Azure template reference](https://learn.microsoft.com/azure/templates/) documents it.

- `apiVersion` is required, such as `"2023-09-01"`.
- `name` defaults to the `id`.
- `type` and `dependsOn` are not params.
- A child type has a name with as many parts (`Microsoft.Sql/servers/databases` is
  `my-server/orders`).
- An extension resource (lock, role assignment) uses `scope`.

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

Params are the declaration: `type` (`string`, `securestring`, `int`, `bool`, `object`,
`secureObject`, `array`) and optional `defaultValue`, `allowedValues`, `minValue`,
`maxValue`, `minLength`, `maxLength`, `metadata`, `nullable`. Read it with
`${admin_password}`; see [Giving parameters their values](#giving-parameters-their-values).

A secure parameter with a written default warns: it would be stored in the template.

## References

Each `${...}` becomes the ARM expression that reads it.

| You write | The template gets | Where |
|---|---|---|
| `${web_vnet}` or `${web_vnet.id}` | `[resourceId('Microsoft.Network/virtualNetworks', 'web-vnet')]` | anywhere |
| `${web_vnet.name}` | `web-vnet` | anywhere |
| `${web_vnet.type}`, `${web_vnet.apiVersion}` | the type and the API version, as text | anywhere |
| `${web_ip.properties.ipAddress}` | `[reference(resourceId(...), '2023-09-01').ipAddress]` | `properties` and outputs |
| `${web_vm.identity.principalId}` | `[reference(resourceId(...), '2023-09-01', 'Full').identity.principalId]` | `properties` and outputs |
| `${admin_password}` | `[parameters('admin_password')]` | anywhere |
| `${names[0]}` | `[parameters('names')[0]]` | anywhere |

Values known at compile are written out. At subscription scope `resourceId` is
`subscriptionResourceId`; a scoped resource uses `extensionResourceId`.

State (`reference()`) can be read only in `properties` and outputs; elsewhere, or a resource
reading itself, is refused.

### References inside text

A reference inside longer text becomes one `format()`:

```yaml
subnet:
  id: ${web_vnet}/subnets/web
# "[format('{0}/subnets/web', resourceId('Microsoft.Network/virtualNetworks', 'web-vnet'))]"
```

Write `$${` for a literal `${`.

### ARM expressions

A string in square brackets is an ARM expression, passed through after a syntax check:

```yaml
location: "[resourceGroup().location]"
```

A reference inside an expression is put in place as an argument:

```yaml
name: "[concat('weblogs', uniqueString(resourceGroup().id, ${web_vnet.name}))]"
# "[concat('weblogs', uniqueString(resourceGroup().id, 'web-vnet'))]"
keys: "[listKeys(${logs}, ${logs.apiVersion}).keys[0].value]"
```

`[[` at the start is a literal `[`; it is doubled back only where needed.

Values from `--data`, `--set`, data layers or `env()` are never expressions
([see](../reference/dsl/interpolation.md#a-tools-own-expressions)): `[listKeys(...)]` from
data is deployed as text.

## Ordering

The plugin writes ARM `dependsOn` for every reference and every document `dependsOn`.

```yaml
- id: web_vm
  type: Microsoft.Compute/virtualMachines
  params: { ... }
  dependsOn:
    - resource: web_nsg   # nothing in the params mentions it
```

Cycles are refused: `a -> b -> c -> a`.

## Outputs

Each output becomes a template output.

```yaml
outputs:
  - name: web_vm_ip
    value: ${web_ip.properties.ipAddress}
  - name: web_url
    value: http://${web_ip.properties.dnsSettings.fqdn}/
```

Every output is a `string`; other values go through `string()` (objects as JSON).

## `notifies`, `runtimeWhen` and `state`

All refused:

- `notifies` on any resource, whatever the action: `unsupported-action`.
- `runtimeWhen`: `unsupported-runtime-condition`. ARM's `condition` param passes through.
- `state: absent`, or any state other than `present`: `unsupported-state`.

## Plan, apply and outputs

| Operation | What the plugin runs |
|---|---|
| Plan | `az deployment group what-if --name <name> --resource-group <rg> --template-file template.json --mode <mode> --no-pretty-print` |
| Apply | the same preview, so the changes can be reported, then `az deployment group create` with the same arguments |
| Outputs later | `az deployment group show --name <name> --resource-group <rg>` |

At subscription scope: `az deployment sub ...` with `--location`. `--subscription` is added
when named. Every call uses `--output json`.

What-if results:

| Azure says | Meridian reports |
|---|---|
| `Create` | create |
| `Delete` | delete |
| `Modify` | update, listing up to five of the properties that change |
| `Deploy` | update, saying that the resource is deployed again and the preview cannot tell what changes |
| `NoChange`, `Ignore` | nothing. `Ignore` is a resource that exists, is not in the template, and is left alone |
| `Unsupported` | nothing, and the plan is marked unknown, so it is never reported as in sync |

Changes use your ids; unknown names use the Azure id.

An apply always creates the deployment. A failed preview deploys nothing. Azure does not
roll back a failed deployment. Secure outputs are sensitive and not returned. Credentials
are `az login`'s.

CI parses every template with `az bicep decompile`. The runner has only met a stand-in
`az`; the real job needs four repository settings
([Real-tool tests](../contributing/real-tool-tests.md#azure)). Treat it as untested against
Azure until then.

### Environment

| Variable | Meaning |
|---|---|
| `MERIDIAN_AZ` | The program to run, when it is not `az` on `PATH` |
| `MERIDIAN_AZ_ARGS` | Extra arguments for every call, split like a shell command line (quotes hold a word together), such as `--only-show-errors` |
| `MERIDIAN_AZ_PARAMETERS` | Values for the template's parameters, split like a shell command line (quotes hold a word together) |

Settings can also go in `meridian.yaml` under `spec.settings.arm`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#arm).

### Giving parameters their values

- **From the operator:** each word of `MERIDIAN_AZ_PARAMETERS` goes to `--parameters`.

  ```sh
  export MERIDIAN_AZ_PARAMETERS="@prod.parameters.json admin_user=ops"
  ```

- **From another document:** an [import](../reference/dsl/outputs-and-hand-offs.md#imports)
  named like a parameter sets it.

Operator values and secure parameters are never logged.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) becomes a `securestring`
parameter read from a private file.

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

The param becomes `[parameters('meridian_secret_db_password')]` (or a `format()`):

```json
"parameters": {
  "meridian_secret_db_password": { "type": "securestring" }
}
```

The runner writes a parameters file (mode `0600`, in a `0700` directory, in memory where
possible), passed as `--parameters @<path>` to `what-if` and `create` before other values,
and removed after.

Azure does not return a `securestring` in the deployment record. The plugin reads only ids,
kinds and changed property names from what-if. Whether what-if prints secure values in a
diff is unchecked against a real subscription.

A secret in `deployment` settings or a `parameter` declaration is refused
(`unsupported-secret-place`).

Tested against a stand-in `az` (`plugins/arm/tests/run.rs`) and the Bicep reader in CI
(`plugins/arm/tests/real.rs`).

## What is refused

Each refusal names the resource, line and rule:

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

`meridian schema sync` reads the same
[Azure/bicep-types-az](https://github.com/Azure/bicep-types-az) definitions as
[Bicep](bicep.md#editor-schemas), keyed by `type` and the `apiVersion` param
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `Namespace/type` types the documents use, at the API version the resource's `apiVersion` names, or the newest stable one when it names none: every property as for Bicep, plus `apiVersion` itself, required, with every API version the index has as its values |
| Listed by name | every other type in the index, once, with its `apiVersion` values |
| Pinned | the index and each `types.json` read, to their SHA-256 |

With two API versions of one type, the first is used.

## Not covered yet

- Deployments to a management group or a tenant.
- Template `variables` and user-defined `functions`.
- Typed outputs.
- Compile-time property checks ([Editor schemas](#editor-schemas) check them).

## Examples

| Document | What it shows |
|---|---|
| [`examples/arm/web-vm.yaml`](../../examples/arm/web-vm.yaml) | A virtual machine with its network, a secret parameter, every kind of reference |
| [`examples/arm/sql-database.yaml`](../../examples/arm/sql-database.yaml) | Child resources, names made from a parameter, an extension resource, mode `Complete` |
| [`examples/arm/landing-zone.yaml`](../../examples/arm/landing-zone.yaml) | A deployment to a subscription |

Compiled output: [`plugins/arm/testdata`](../../plugins/arm/testdata).

## ARM and Bicep

Use `arm` when you need the JSON template itself; otherwise see [Bicep](bicep.md).
