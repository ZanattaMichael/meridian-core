# Target: `bicep`

<img src="../branding/logos/meridian-bicep.svg" width="72" alt="Meridian Bicep plugin logo">

Compiles an Infrastructure document into a Bicep file, deployed to a resource group with
`az` (`what-if`, then `create`).

## What it does and does not support

| | |
|---|---|
| Category | Infrastructure as code |
| Resource types | Azure's own, with the API version: `Namespace/type@api-version`, plus `deployment` for where the file is deployed. Also the [common infrastructure types](#common-infrastructure-types) |
| `dependsOn` | Yes, as Bicep's `dependsOn`, unless a reference already implies the order |
| `notifies` | No. An Azure deployment has no way to react to a change in another resource. Refused |
| `runtimeWhen` | No. Refused |
| `adopt` | Accepted, and changes nothing: Resource Manager updates a resource of the same name in place. The compile warns |
| `state: absent` | No. A resource group deployment never deletes a resource; remove it from the document and delete it in Azure |
| References | `${id}` is the resource's symbolic name, `${id.properties.x}` a property read from it, anything else Bicep's own expression language; see [References and expressions](#references-and-expressions) |
| Plan | Yes: `what-if`. Where Azure cannot analyse a resource type, the plan is reported as unknown |
| Apply | Yes: the same preview, then `create` |
| Outputs | Yes, always as strings |
| Destroy | Yes: each resource the deployment's record lists is deleted, then the record. See [Destroying](../operations/destroying.md) |
| Editor schemas | Yes: `meridian schema sync` reads the Azure type definitions Bicep compiles against, published as files; no account and no compiler. See [Editor schemas](#editor-schemas) |
| Compiles to | `main.bicep` and `deployment.json` |
| Runs with | `az deployment group` |

Not supported:

- **Bicep parameters, variables and modules.** Values are settled from `--data`, `--set`
  and data layers.
- **Scopes other than a resource group.**
- **Per-type checks at compile.** The Bicep compiler checks types when `az` builds.
- **Deleting.** An empty document is refused.

It also takes the [common infrastructure types](#common-infrastructure-types).

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | the `az` command line, signed in (`az login`), with the Bicep compiler it installs itself (`az bicep install`) |
| In Azure | the resource group, which must already exist, and permission to deploy to it |
| To compile only | Nothing. `meridian compile` never runs `az` |

## A first document

[`examples/bicep/web-vm.yaml`](../../examples/bicep/web-vm.yaml) deploys an address, a
network, an interface and a lock:

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-vm
spec:
  target: bicep
  resources:
    # Not part of the Bicep file: where it is deployed.
    - id: target
      type: deployment
      params:
        resourceGroup: web-prod

    - id: web_ip
      type: Microsoft.Network/publicIPAddresses@2023-09-01
      params:
        name: web-ip
        location: ${resourceGroup().location}
        sku:
          name: Standard
        properties:
          publicIPAllocationMethod: Static
          dnsSettings:
            domainNameLabel: web-${uniqueString(resourceGroup().id)}

    - id: lock
      type: Microsoft.Authorization/locks@2020-05-01
      params:
        name: keep-web-ip
        scope: ${web_ip}
        properties:
          level: CanNotDelete
      # Nothing in the body mentions the interface, so this becomes dependsOn.
      dependsOn:
        - resource: nic

  outputs:
    - name: web_vm_ip
      value: ${web_ip.properties.ipAddress}
    - name: web_url
      value: http://${web_ip.properties.dnsSettings.fqdn}/
```

It compiles to
([the whole file](../../plugins/bicep/testdata/web-vm.main_bicep)):

```bicep
resource web_ip 'Microsoft.Network/publicIPAddresses@2023-09-01' = {
  name: 'web-ip'
  location: resourceGroup().location
  properties: {
    dnsSettings: {
      domainNameLabel: 'web-${uniqueString(resourceGroup().id)}'
    }
    publicIPAllocationMethod: 'Static'
  }
  sku: {
    name: 'Standard'
  }
}

resource lock 'Microsoft.Authorization/locks@2020-05-01' = {
  name: 'keep-web-ip'
  properties: {
    level: 'CanNotDelete'
  }
  scope: web_ip
  dependsOn: [
    nic
  ]
}

output web_vm_ip string = string(web_ip.properties.ipAddress)
output web_url string = 'http://${web_ip.properties.dnsSettings.fqdn}/'
```

## Resources and the deployment

`type` is the Azure type with API version; `params` are the body as Azure documents it.
The API version and `name` are required.

Ids are Bicep symbolic names (a letter, then letters, digits, `_`), sharing a namespace
with outputs.

Exactly one `deployment` resource says where the file goes:

| Param | Meaning | Default |
|---|---|---|
| `resourceGroup` | the resource group to deploy to, which must exist | required |
| `name` | the deployment's name in Azure: letters, digits, hyphens, underscores, periods and parentheses | the document's name |
| `subscription` | the subscription, when it is not the `az` command's current one | the current one |

A second is refused (`duplicate-resource`), as is `dependsOn` on it
(`unsupported-dependency`).

## Common infrastructure types

`network`, `subnet`, `firewall`, `address`, `vm`, `disk` and `objectStore` are Meridian's own types,
and this target turns each into Azure's own resource types
([the common infrastructure types](../reference/dsl/common-infrastructure-types.md)
has every param and mapping). They are mixed freely with Azure's own types and
go to the deployment's resource group, at the group's location unless a
resource gives one.

| Common type | Becomes |
|---|---|
| `network` | `Microsoft.Network/virtualNetworks@2023-09-01` |
| `subnet` | `Microsoft.Network/virtualNetworks/subnets@2023-09-01`, with the network as its `parent` |
| `firewall` | `Microsoft.Network/networkSecurityGroups@2023-09-01`, the rules inside it |
| `address` | `Microsoft.Network/publicIPAddresses@2023-09-01`, Standard and static |
| `vm` | `Microsoft.Compute/virtualMachines@2024-03-01` and a `Microsoft.Network/networkInterfaces@2023-09-01` named `<id>_nic` |
| `disk` | `Microsoft.Compute/disks@2024-03-01`, listed in its vm's `dataDisks` |
| `objectStore` | `Microsoft.Storage/storageAccounts@2023-05-01`, its `blobServices` and a container |

The same document compiles with Terraform for Azure, with the same sizes and
images; compare [`examples/common/web-bicep.yaml`](../../examples/common/web-bicep.yaml)
with [`web-azure.yaml`](../../examples/common/web-azure.yaml).

## References and expressions

| You write | It becomes |
|---|---|
| `${web_ip}` | `web_ip`, the resource's symbolic name |
| `${web_ip.properties.ipAddress}` | `web_ip.properties.ipAddress`, a property read from it |
| either inside longer text | Bicep string interpolation: `'http://${web_ip.properties.dnsSettings.fqdn}/'` |
| `${resourceGroup().location}`, `${uniqueString(...)}` | Bicep's own expression language, handed over as written |
| `$${` | a literal `${` |

Other `${...}` is Bicep's, checked by its compiler; `${toUpper(store.name)}` still orders.
Values from data never become expressions
([see](../reference/dsl/interpolation.md#a-tools-own-expressions)).

Outputs are `string`, wrapped in `string(...)` when needed.

## Ordering

A `dependsOn` a reference implies is left out. Others become `dependsOn`, as for the lock
above. Resources are written in Meridian's order.

## What a compile writes

```
main.bicep
deployment.json
```

`main.bicep` is an ordinary Bicep file. `deployment.json` holds the resource group, name,
subscription and the map back to your ids.

Strings are single-quoted with `\\`, `\'`, `\n`, `\r`, `\t` and `\${` escaped. Keys such
as `cost-center` are quoted.

## Editor schemas

`meridian schema sync` fetches the Bicep compiler's type definitions from
[Azure/bicep-types-az](https://github.com/Azure/bicep-types-az): the index and the
`types.json` files the documents need. No account or compiler is needed
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | the `Namespace/type@api-version` types the documents use: every property with its kind, whether it is required, the values it allows (a union of literals), its nested properties; properties Azure sets, such as `id`, as read-only; write-only properties as sensitive. `type` and `apiVersion` are left out because the type gives them |
| Listed by name | every other API version of a used type, and the newest stable API version of every other type in the index |
| Pinned | the index and each `types.json` read, to their SHA-256 |

- Types match case-insensitively.
- Extensible enums are plain strings.
- Discriminated objects take every variant's properties.
- Files are cached and refetched when changed; offline, the cache is used with a note.
- The [ARM target](arm.md) reads the same files.

## Running

| | |
|---|---|
| Plan | `az deployment group what-if --resource-group <rg> --name <name> --template-file main.bicep --no-pretty-print --output json` |
| Apply | the same preview, so the changes can be reported, then `az deployment group create --resource-group <rg> --name <name> --template-file main.bicep --output json` |
| Outputs later | `az deployment group show --resource-group <rg> --name <name> --output json` |
| Destroy | `az deployment group show` (the record's `outputResources`), then `az resource delete --ids` for each, children first and otherwise last made first, then `az deployment group delete`; `plan --destroy` stops after `show`. A resource already gone is skipped. A resource the deployment did not make, or one made by a nested deployment that the record does not list, is kept |

`--subscription` is added when named. A file the compiler rejects fails the plan with its
diagnostics.

What-if `Create`, `Delete` and `Modify` rows are reported by id. `Deploy` or `Unsupported`
makes the plan **unknown**. `NoChange` and `Ignore` are left out. Secure outputs are never
printed.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) becomes a `@secure()`
parameter read from a private file.

```yaml
spec:
  target: bicep
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_password_secret
      type: Microsoft.KeyVault/vaults/secrets@2023-07-01
      params:
        name: app-vault/db-password
        properties:
          value: ${{ secret.db_password }}
```

The param becomes `meridian_secret_db_password`, declared at the top:

```bicep
@secure()
param meridian_secret_db_password string
```

It becomes a `securestring`, kept out of the deployment record.

The runner writes a parameters file (mode `0600`, in a `0700` directory, in memory where
possible), passed as `--parameters @<path>` to `what-if` and `create`, removed after.

A property keeps the value as its provider does (a tag is not encrypted). The plugin reads
only ids and kinds from what-if. Whether what-if prints secure values in a diff is
unchecked against a real subscription
([Real-tool tests](../contributing/real-tool-tests.md#azure)).

A secret in `deployment` settings is refused (`unsupported-secret-place`).

Tested against a stand-in `az` (`plugins/bicep/tests/run.rs`) and the real compiler in CI
(`plugins/bicep/tests/real.rs`).

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_AZ` | the program to run, when it is not `az` on `PATH` |
| `MERIDIAN_AZ_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--only-show-errors`, or `--subscription <id>` for every document |

Settings can also go in `meridian.yaml` under `spec.settings.bicep`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#bicep). Credentials are
`az login`'s.

Meridian takes no [cloud credential](../reference/dsl/credentials.md#reaching-a-cloud-as-a-credential)
for `bicep` yet: `az login --service-principal` takes the secret on its command line, so a
document that names one is refused before anything runs. Declare it `from: ambient` to name
the signed-in `az`.

## What is refused

Each names the resource, line and rule:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type without `@api-version`, or not of the form `Namespace/type` | write the Azure type with its API version |
| `missing-resource` | no resource of type `deployment`, or one without `resourceGroup` | add one with the resource group |
| `duplicate-resource` | two resources of type `deployment` | keep one |
| `invalid-name` | a deployment name Azure does not accept | letters, digits, hyphens, underscores, periods and parentheses |
| `unsupported-param` | a param the `deployment` resource does not take | only `resourceGroup`, `name` and `subscription` |
| `invalid-param` | a `deployment` param that is not a string | quote it |
| `missing-param` | a resource without `name` | add the resource's name in Azure |
| `invalid-name` | a resource or output id that cannot be a Bicep name | start with a letter; use letters, digits and underscores |
| `duplicate-resource` | an output with the same name as a resource | rename one; Bicep keeps both in one namespace |
| `duplicate-output` | two outputs with the same name | rename one |
| `invalid-reference` | `${id.x}` where `id` is a resource and the path cannot be read | fix the path |
| `unsupported-state` | `state: absent`, or any state but `present` | remove the resource from the document, and delete it in Azure |
| `unsupported-runtime-condition` | `runtimeWhen` on any resource | decide with a compile-time `when` instead |
| `unsupported-action` | `notifies` on any resource | drop it; a deployment has no notifications |
| `unsupported-dependency` | a `dependsOn` to or from the `deployment` resource | the deployment is not a resource of the file |
| `empty-document` | a document with no resources but the deployment | add a resource |

## Reading a failure

When the deployment fails, `az deployment operation group list` is read. Each operation
with `provisioningState: Failed` is a [resource failure](../reference/state.md#resource-failures):

| From the operation | Becomes |
|---|---|
| `targetResource` | the resource, the document's id where it maps to one |
| `statusMessage.error.message` | the summary |
| `statusMessage.error.code`, else `statusCode` | the tool code, `StorageAccountAlreadyTaken` |
| `Unauthorized`, `Forbidden`, `AuthorizationFailed` | `auth-failed` |
| `InvalidTemplate...`, `BadRequest` | `invalid-config` |
| a `...NotFound` code other than `ResourceNotFound` | `dependency-missing` |
| anything else | `resource-failed`, or `unreachable`, `auth-failed`, `timed-out` or `reboot-required` when the message says so |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run az: it is not installed, or not on the PATH` | the plugin runs `az` on the machine running Meridian | install the Azure CLI, or set `MERIDIAN_AZ` |
| `Please run 'az login'` | the `az` command is not signed in | `az login`, or a service principal's `az login --service-principal` |
| `ResourceGroupNotFound` | a resource group deployment needs the group to exist | create it: `az group create --name <rg> --location <region>` |
| a Bicep diagnostic such as `BCP035` or `BCP036` | a property is missing or of the wrong type for the resource's API version | fix the resource's body; the diagnostic names the property |
| the plan is unknown | what-if could not analyse a resource type, or will deploy one without saying whether it changes | expected for some types; the apply's outputs say what was deployed |
| `az could not preview the deployment` | what-if failed before it could analyse; the reason is Azure's | read the output above the message |
| an output is reported as sensitive and not printed | it is of a secure type | expected; read it with `meridian outputs` into a stage that needs it |

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--verbose` to `az` at level 1 and `--debug` from level 2.

## How it is tested

- **Golden files** in `plugins/bicep/testdata/`, refusals included.
- **Run tests** (`plugins/bicep/tests/run.rs`) against a stand-in `az`.
- **Real-tool tests** (`plugins/bicep/tests/real.rs`, `azure.yml`): the real compiler builds
  every output on each pull request. The runner's subscription job needs four repository
  settings ([Real-tool tests](../contributing/real-tool-tests.md#azure)). Run with
  `MERIDIAN_TEST_REAL_BICEP=1 cargo test -p meridian-target-bicep --test real -- --ignored`.
  Treat the runner as untested against Azure until that job has run.
