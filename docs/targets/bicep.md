# Target: `bicep`

<img src="../branding/logos/meridian-bicep.svg" width="72" alt="Meridian Bicep plugin logo">

Compiles an Infrastructure document into an Azure Bicep file and deploys it to
a resource group with the `az` command line. Bicep and Azure still do the
work: Meridian writes the file a person would have written, and `az` builds
it, previews it with `what-if` and deploys it.

## What it does and does not support

| | |
|---|---|
| Category | Infrastructure as code |
| Resource types | Azure's own, with the API version: `Namespace/type@api-version`, plus `deployment` for where the file is deployed |
| `dependsOn` | Yes, as Bicep's `dependsOn`, unless a reference already implies the order |
| `notifies` | No. An Azure deployment has no way to react to a change in another resource. Refused |
| `runtimeWhen` | No. Refused |
| `adopt` | Accepted, and changes nothing: Resource Manager updates a resource of the same name in place. The compile warns |
| `state: absent` | No. A resource group deployment never deletes a resource; remove it from the document and delete it in Azure |
| References | `${id}` is the resource's symbolic name, `${id.properties.x}` a property read from it, anything else Bicep's own expression language; see [References and expressions](#references-and-expressions) |
| Plan | Yes: `what-if`. Where Azure cannot analyse a resource type, the plan is reported as unknown |
| Apply | Yes: the same preview, then `create` |
| Outputs | Yes, always as strings |
| Editor schemas | Yes: `meridian schema sync` reads the Azure type definitions Bicep compiles against, published as files; no account and no compiler. See [Editor schemas](#editor-schemas) |
| Compiles to | `main.bicep` and `deployment.json` |
| Runs with | `az deployment group` |

What it does not do:

- **No Bicep parameters, variables or modules.** Values come from the
  document, through Meridian's own `--data`, `--set` and data layers, and are
  written into the file settled. The compiled file takes no parameters.
- **Resource group scope only.** A document deploys to one resource group;
  subscription, management group and tenant deployments are not written.
- **No per-type checking at compile time.** The compile checks the shape of
  the document; the Bicep compiler checks each resource against Azure's
  published types when `az` builds the file, and a file it rejects fails the
  plan with its reason.
- **It deletes nothing.** An empty document is refused; delete a resource in
  Azure yourself.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resources and the deployment](#resources-and-the-deployment) ·
[References and expressions](#references-and-expressions) ·
[Ordering](#ordering) ·
[What a compile writes](#what-a-compile-writes) ·
[Editor schemas](#editor-schemas) ·
[Running](#running) ·
[Settings](#settings) ·
[What is refused](#what-is-refused) ·
[Troubleshooting](#troubleshooting) ·
[How it is tested](#how-it-is-tested)

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | the `az` command line, signed in (`az login`), with the Bicep compiler it installs itself (`az bicep install`) |
| In Azure | the resource group, which must already exist, and permission to deploy to it |
| To compile only | Nothing. `meridian compile` never runs `az` |

## A first document

[`examples/bicep/web-vm.yaml`](../../examples/bicep/web-vm.yaml) deploys a
public address, a network, an interface and a lock. The part that shows what
is particular to Bicep:

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

A resource's `type` is the Azure resource type with its API version, and its
`params` are the resource's body exactly as Azure documents it: `name`,
`location`, `sku`, `properties`, `tags`, `scope` and the rest. A type without
its API version is refused, and so is a resource without a `name`: the name
is what the resource is called in Azure, and Bicep requires it.

A resource's id is its symbolic name in the Bicep file, so it starts with a
letter and holds only letters, digits and underscores. An output's name is a
Bicep name too, and shares one namespace with the resources.

One resource **must** have the type `deployment`. It is not part of the Bicep
file. It says where the file goes:

| Param | Meaning | Default |
|---|---|---|
| `resourceGroup` | the resource group to deploy to, which must exist | required |
| `name` | the deployment's name in Azure: letters, digits, hyphens, underscores, periods and parentheses | the document's name |
| `subscription` | the subscription, when it is not the `az` command's current one | the current one |

A second `deployment` resource is refused (`duplicate-resource`), and a
`deployment` may not take part in `dependsOn` (`unsupported-dependency`).

## References and expressions

| You write | It becomes |
|---|---|
| `${web_ip}` | `web_ip`, the resource's symbolic name |
| `${web_ip.properties.ipAddress}` | `web_ip.properties.ipAddress`, a property read from it |
| either inside longer text | Bicep string interpolation: `'http://${web_ip.properties.dnsSettings.fqdn}/'` |
| `${resourceGroup().location}`, `${uniqueString(...)}` | Bicep's own expression language, handed over as written |
| `$${` | a literal `${` |

Anything between `${` and `}` that is not a path into a resource of the
document is Bicep's, and is checked by the Bicep compiler, not by Meridian. A
function call that mentions a resource, such as `${toUpper(store.name)}`,
still orders the two. Values from `--data`, `--set` and data layers are
settled before the file is written and never become expressions
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

An output is always declared `string`; a value that is not already a string
is wrapped in `string(...)`.

## Ordering

A reference already orders two resources, so a `dependsOn` that a reference
implies is left out of the file; Bicep's own linter would flag it. Only an
order nothing in the body says becomes `dependsOn`, as the lock on the
interface does above. The resources are written in the order Meridian
decided, which Bicep does not need but a reader does.

## What a compile writes

```
main.bicep
deployment.json
```

`main.bicep` is an ordinary Bicep file, so you can build it with `az bicep
build` or deploy it from a pipeline of your own. `deployment.json` holds what
the runner needs and the file does not say: the resource group, the
deployment's name and subscription, and the map from each resource's Azure
type and name back to the id you wrote, so a what-if row is reported by your
name.

Strings are written as single-quoted Bicep strings, with `\\`, `\'`, `\n`,
`\r` and `\t` escaped and a `${` in literal text written `\${`, so only
what the document meant as an expression is one. A key that is not a plain
identifier, such as `cost-center`, is quoted.

## Editor schemas

`meridian schema sync` reads the type definitions the Bicep compiler itself
compiles against, so an editor completes and checks a resource's properties
at its API version ([Tool schemas](../tooling/schema-sync.md)). They are
published as files in
[Azure/bicep-types-az](https://github.com/Azure/bicep-types-az), generated
from the Azure REST API specifications: an index of every
`Namespace/type@api-version`, and a `types.json` per provider and version
holding the type graph. The sync fetches the index and the files the
documents' types live in; no account and no compiler is needed.

| | |
|---|---|
| Described in full | the `Namespace/type@api-version` types the documents use: every property with its kind, whether it is required, the values it allows (a union of literals), its nested properties; properties Azure sets, such as `id`, as read-only; write-only properties as sensitive. `type` and `apiVersion` are left out because the type gives them |
| Listed by name | every other API version of a used type, and the newest stable API version of every other type in the index |
| Pinned | the index and each `types.json` read, to their SHA-256 |

A type is matched without regard to case, as Azure matches resource types.
An extensible enum, literals beside a plain string, is a string with no
values listed, because Azure takes values beyond the known ones. A
discriminated object, whose properties depend on a `kind`, takes the base
properties and every variant's, open. The files are kept in the work
directory and fetched again only when they changed; when GitHub cannot be
reached, the last copy is used and the sync says so. The source is tested
against the real files in the `azure` workflow. The [ARM target](arm.md)
reads the same files.

## Running

| | |
|---|---|
| Plan | `az deployment group what-if --resource-group <rg> --name <name> --template-file main.bicep --no-pretty-print --output json` |
| Apply | the same preview, so the changes can be reported, then `az deployment group create --resource-group <rg> --name <name> --template-file main.bicep --output json` |
| Outputs later | `az deployment group show --resource-group <rg> --name <name> --output json` |

`--subscription` is added when the `deployment` resource names one. `az`
builds the Bicep file itself on every call, so a file the Bicep compiler
rejects fails the plan with the compiler's own diagnostics.

Changes are read from what-if's JSON, resource by resource: `Create`,
`Delete` and `Modify` are reported under the id you wrote. A row Azure marks
`Deploy` (it will deploy the resource but cannot say whether that changes it)
or `Unsupported` (it cannot analyse the type) makes the plan **unknown**, not
empty, so a run never says "nothing to do" on no evidence. `NoChange` and
`Ignore` rows are left out.

An apply reports the preview's changes and the deployment's outputs. An
output of a secure type is reported as sensitive and never printed.

## Secrets

A [secret](../reference/dsl/secrets.md#handing-a-secret-to-a-tool) in a resource's
params reaches Azure as a `@secure()` parameter whose value is read from a private
file, and through nothing else.

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

**What the compile writes.** The param becomes the parameter's name,
`meridian_secret_db_password`, and the file declares the parameter at the top:

```bicep
@secure()
param meridian_secret_db_password string
```

The compiler makes a `securestring` parameter of it, so Resource Manager keeps the
value out of the deployment's record and its logs. The compiled file holds the
parameter's name and never its value.

**How the value reaches the tool.** When a stage runs, the runner writes every secret
into one Resource Manager parameters file (`"parameters": {"<name>": {"value": ...}}`)
in a directory only the user can enter (mode `0700`, the file `0600`, on a memory
filesystem where the system has one), and passes it as `--parameters @<path>` to both
`what-if` and `create`. The file is overwritten and removed when the stage ends. The
command line names the file's path and nothing more, and `az`'s environment holds
nothing of the value.

**What Azure keeps.** A secure parameter's value is not in the deployment's record and
not returned by `show`. A resource property given the value is kept by the resource
provider the way it keeps that property: a Key Vault secret is encrypted at rest; a
tag is not. The plugin reads only each change's resource id and kind from the preview,
never a property's value. Whether Azure's what-if prints a secure parameter's value
inside a property diff is not something the tests have checked: the job that runs
against a real subscription is switched on by the repository's settings, see
[Real-tool tests](../contributing/real-tool-tests.md#azure).

**What is refused.** A secret in the `deployment` resource's settings is refused at
compile, rule `unsupported-secret-place`: the settings are the command line `az` is
given.

The handover is tested against a stand-in `az` that keeps a copy of the parameters
file, its mode, its arguments and its environment (`plugins/bicep/tests/run.rs`), and
the compiled parameter is built by the real Bicep compiler in CI
(`plugins/bicep/tests/real.rs`), which makes a `securestring` of it.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_AZ` | the program to run, when it is not `az` on `PATH` |
| `MERIDIAN_AZ_ARGS` | arguments added to every call, split like a shell command line (quotes hold a word together): `--only-show-errors`, or `--subscription <id>` for every document |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.bicep`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#bicep).

Credentials are the `az` command's own (`az login`).

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

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

## How it is tested

- **Golden files** under `plugins/bicep/testdata/` record what the example
  compiles to, `main.bicep` and `deployment.json`, byte for byte, and what
  each refusal says.
- **Run tests** (`plugins/bicep/tests/run.rs`) drive the runner against a
  stand-in for `az`: the exact calls, a what-if read back with every change
  type, the rows that make a plan unknown, and the outputs.
- **Real-tool tests** (`plugins/bicep/tests/real.rs`, workflow `azure.yml`)
  have two parts. On every pull request that touches the plugin, the real
  Bicep compiler builds what the plugin writes, the example and every kind of
  value, and checks each resource against Azure's published types; a file the
  compiler rejects is reported by the compiler. The runner itself (`what-if`,
  `create` and `show`) is tested in a real subscription by a job that is
  switched on by four repository settings, listed under
  [Real-tool tests](../contributing/real-tool-tests.md#azure). Both are
  marked `#[ignore]` and run with
  `MERIDIAN_TEST_REAL_BICEP=1 cargo test -p meridian-target-bicep --test real -- --ignored`.
  Until the subscription job has run, treat the runner as untested against
  Azure.
