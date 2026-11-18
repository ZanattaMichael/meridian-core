# Target: `dsc`

<img src="../branding/logos/meridian-dsc.svg" width="72" alt="Meridian DSC plugin logo">

Compiles a ResourceSet into a DscPipelineRunner configuration and runs it with
`Invoke-DscRunner`.

DscPipelineRunner drives both generations of DSC through one configuration
format: DSC v2 through `Invoke-DscResource`, and DSC v3 through the `dsc`
command line. So this one target covers both, and which engine evaluates the
resources is a setting.

## What it does and does not support

| | |
|---|---|
| Resource types | DSC's own: `Module/Resource`, such as `PSDscResources/Service` or `Microsoft.Windows/Registry`. The abstract types (`package`, `service`, ...) are not taken |
| Settings | `spec.settings`: `engine`, `target` and `credential`, [how the configuration is run](#how-the-configuration-is-run) |
| `dependsOn` | Yes, as the runner's own `dependsOn` |
| `notifies` | Native: the runner's `notify`. The action is `refresh`: when the notifier really changed, the notified resource's `Set` runs even if its `Test` says there is nothing to do |
| `runtimeWhen` | Yes: a `preCondition` |
| `adopt` | Accepted, and changes nothing: DSC keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
| `state` | No. A DSC resource carries its desired state in its own properties (`Ensure`, `_exist`), so set it in `params` |
| Plan | Yes: the runner's `Test` mode |
| Outputs | No |

## What a document looks like

A resource's `type` is the DSC resource as the runner names it and its `params`
are the resource's properties, exactly as the resource documents them. Which
module a resource lives in, and whether it is a v2 or a v3 resource, is DSC's
business: `Module/Resource` is the same shape for both, and only the engine can
tell whether a type exists.

### How the configuration is run

How the configuration is run is not a resource: it is said once, for the whole
document, under [`spec.settings`](../reference/dsl/index.md#settings). Each
value is text, and the keys are the runner's own `PipelineRunnerSettings`:

| Setting | Meaning |
|---|---|
| `engine` | `DscV2`, `DscV3`, or `Auto` (the default: v3 when `dsc` is installed) |
| `target` | `Local`, `WinRM` or `SSH`: how the runner reaches the document's host |
| `credential` | the name of a credential the runner resolves itself. The secret never appears in the document |

```yaml
spec:
  target: dsc
  targetHost:
    from: inline
    host: web01.example.com
  settings:
    engine: DscV2
    target: WinRM
    credential: svcDeploy
  resources:
    - id: site_config
      type: PSDesiredStateConfiguration/File
      params:
        DestinationPath: C:\inetpub\app\web.config
        Ensure: Present
```

A value outside the choices is refused (`invalid-setting`), and a key not in
the table is refused by Meridian before the plugin is asked:
`the document sets spec.settings.transport, and target "dsc" takes only:
credential, engine, target`. Interpolation works in a setting as it does in a
param, so `target: "${{ var.transport }}"` is fine.

Before `spec.settings` existed, the same three were the params of one resource
of type `runner`. A document written that way still compiles, with a warning
that names the keys to move, until the next `apiVersion` removes the type. Where
the resource and `spec.settings` give the same key different values the
document is refused (`conflicting-settings`): the settings are said once. The
resource is not a DSC resource, so a `runtimeWhen` on it is refused too
(`unsupported-runtime-condition`).

### The version of DSC

The plugin declares the versions `2` and `3`, and a document's
[`targetVersion`](../reference/dsl/index.md#targetversion) chooses the engine:

| `targetVersion` | Engine |
|---|---|
| not set | `spec.settings.engine`, or `Auto` |
| `"2"` (or `"2.x"`) | `DscV2` |
| `"3"` (or `"3.x"`) | `DscV3` |

```yaml
spec:
  target: dsc
  targetVersion: "3"
```

Say it once. A document whose `targetVersion` means one engine and whose
`spec.settings.engine` names another (including `Auto`) is refused with the rule
`conflicting-settings`; it is not resolved in favour of either. Any other version
is refused by Meridian before the plugin is asked.

When the document's host is another machine, `spec.settings.target` must be
given (`missing-param`). The runner cannot guess between WinRM and SSH, and
Meridian will not guess for it.

## References

`${id.Property}` reads a property of another resource as DSC reports it, once
that resource has run. It becomes the runner's `reference()` accessor, and the
resource that reads is ordered after the one it reads.

The runner treats every property as a PowerShell expandable string. A `$` or a
backtick you write in a param is escaped, so it reaches the resource as written.

## Conditions

`runtimeWhen` is a `preCondition`: a PowerShell predicate in the runner's
function language, evaluated just before the resource.

```yaml
runtimeWhen: "equals (nodeName) 'web'"
```

A condition guards a resource. How the configuration is run is a setting, so
there is nothing a condition on it could mean; put the condition on the
resources it should guard.

## Example

[`examples/dsc/web-server.yaml`](../../examples/dsc/web-server.yaml) compiles to a
one-node project:
[`Datum.yml`](../../plugins/dsc/testdata/web-server.Datum_yml) and
[`Projects/Present/web.yml`](../../plugins/dsc/testdata/web-server.Projects_Present_web_yml).
One resource of it:

```yaml
- id: site_config
  type: PSDesiredStateConfiguration/File
  params:
    DestinationPath: C:\inetpub\app\web.config
    Contents: <configuration><system.webServer/></configuration>
    Ensure: Present
  dependsOn:
    - resource: site_root
  notifies:
    - resource: w3svc
      action: refresh
```

```yaml
- name: site_config
  type: PSDesiredStateConfiguration/File
  dependsOn:
    - PSDesiredStateConfiguration/File/site_root
  notify:
    - PSDscResources/Service/w3svc
  target:
    action: WinRM
    computerName: web01.example.com
    credentialRef: svcDeploy
  properties:
    Contents: <configuration><system.webServer/></configuration>
    DestinationPath: C:\inetpub\app\web.config
    Ensure: Present
```

`Invoke-DscRunner` takes a Datum project and compiles it before running
anything. Meridian has already done the resolving Datum is for, so the project
it writes has one layer and one node.

## Importing

`meridian import` reads DSC configurations written three ways and writes one ResourceSet
for each node. [Importing existing configuration](../operations/importing.md) is the guide.

| Source | Recognised by | Document |
|---|---|---|
| A PowerShell `Configuration` block | `*.ps1` | `targetVersion: "2"`, one document for each `Node` block |
| A DSC v3 configuration document, YAML or JSON | `*.dsc.yaml`, `*.dsc.yml`, `*.dsc.json`, `*.dsc.config.*`; another name needs `--from dsc` | `targetVersion: "3"` |
| A DscPipelineRunner project, which is what this target compiles to | `Datum.yml` and the node files beside it; import the directory with `-r --from dsc` | the engine and target of the project's `PipelineRunnerSettings` |

In all three, an instance becomes a resource of type `Module/Resource` whose params are
the instance's properties and whose id is the instance's name. A name that is not letters,
digits and underscores, such as `Launch key`, becomes one (`launch_key`), with a note.

### A PowerShell configuration

A configuration script is a program, and importing does not run it. What is read is what
is *declared*: the `Configuration` block, `Import-DscResource`, `Node`, and resource
blocks whose properties are literals.

| In the script | In the document |
|---|---|
| `Node 'web01.example.com' { ... }` | the document's host |
| Resources outside any `Node` | a document for `localhost` |
| `WindowsFeature IIS { ... }` | a resource with the id `IIS`. The module is worked out from `Import-DscResource`: a resource PowerShell ships with is `PSDesiredStateConfiguration/...`, or `PSDscResources/...` when that module is imported and has it; anything else belongs to the one other module imported |
| `'text'`, `"text"`, here-strings, numbers (`512mb` is the number of bytes), `$true`, `$false`, `$null`, `@( ... )`, `'a', 'b'`, `@{ ... }` | the same value |
| `DependsOn = '[WindowsFeature]IIS'` | `dependsOn` |

What is reported:

| In the script | What happens | Level |
|---|---|---|
| A property that is computed: a variable, `"$env:TEMP\x"`, `1 + 2`, a call | Carried as the text it was written as, which is not what it meant, and marked: write the value in | not translated |
| `param( ... )` | Left out. A property that reads a parameter is marked as computed | not translated |
| `if`, `foreach` and any other statement in the configuration | Not read, nor any resource declared inside it. The marker quotes the statement and its line | not translated |
| `Node $AllNodes.NodeName`, or a list of names | No host is written and the document is marked: a document configures one host that it names | not translated |
| A `DependsOn` that names no resource of the configuration | Left out | not translated |
| A machine other than this one | The configuration does not say how it is reached, so `spec.settings.target` is set to `WinRM`, which is how a PowerShell configuration is pushed. Change it to `SSH` if that is how the machine is managed | changed |
| A resource whose module cannot be told, because several modules are imported or none | The type names the first candidate, and says so | changed |
| What the script does outside the `Configuration` block, such as compiling and starting it | Not read: Meridian does both | note |

### A DSC v3 document

`resources` are imported as they are. `dependsOn: ["[resourceId('Type', 'name')]"]`
becomes `dependsOn`. A document names no machine, so it configures `localhost`.

A string property written `[[text]` is the literal `[text]`. A property that is a DSC
expression, such as `[parameters('x')]` or `[reference(...)]`, is carried as text and
marked as not translated, as are the document's own `parameters`, `variables` and
`metadata`.

### A runner project

This is the target's own output read backwards: `dependsOn`, `notify` (as `notifies` with
the action `refresh`) and `preCondition` (as `runtimeWhen`) come back, the accessor
`$((reference 'id').Path)` comes back as `${id.Path}`, and the escaping the compiler added
is removed. The `target` each instance carries becomes the document's host and its
`spec.settings`. A property with any other PowerShell expression in it is marked.

Importing what the target compiled gives a document that compiles to the same files, byte
for byte, and a test holds it to that.

The round-trip tests are in [`plugins/dsc/tests/import.rs`](../../plugins/dsc/tests/import.rs).
[`iis/WebServer.ps1`](../../plugins/dsc/testdata/import/iis/WebServer.ps1) is imported,
compiled, and compared by meaning with
[`iis.expected.node.yml`](../../plugins/dsc/testdata/import/iis.expected.node.yml), the
same configuration written by hand in the runner's format; the imported document is
[`iis.meridian.yaml`](../../plugins/dsc/testdata/import/iis.meridian.yaml).
[`machine.dsc.yaml`](../../plugins/dsc/testdata/import/machine.dsc.yaml) is the v3
document, and [`fleet.meridian.yaml`](../../plugins/dsc/testdata/import/fleet.meridian.yaml)
is what a script full of computed values imports as.

## Editor schemas

`meridian schema sync` asks DSC what each resource's properties are, so an
editor completes and checks them ([Tool schemas](../tooling/schema-sync.md)).
Both generations answer: a PowerShell module's resources are read with
`Get-DscResource -Module` (in the `pwsh` that `MERIDIAN_PWSH` names), or,
where that lists nothing, as it does on Linux for a class-based module, from
the module's `[DscResource()]` classes themselves, which give the same
properties, types, key and mandatory flags and enum values; and when the `dsc` command is installed (or `MERIDIAN_DSC` names it) its own
resources are read with `dsc resource list` and `dsc resource schema`.

| | |
|---|---|
| Described in full | every resource of each PowerShell module the documents use, from its property types (`[string]`, `[Int32]`, `[string[]]`, `[PSCredential]`), which are mandatory, and the values an enum property allows; and each DSC v3 resource the documents use, from the JSON Schema its manifest declares |
| Listed by name | every other DSC v3 resource `dsc resource list` reports |
| Pinned | PowerShell's version, each module's version, each DSC v3 resource's version |

A module that is not on `PSModulePath`, a type neither PowerShell nor
`dsc` describes, and every error PowerShell raised while listing are
reported in notes.

## Running

| | |
|---|---|
| Plan | `Invoke-DscRunner -Mode Test`: every resource is tested, none is set. A resource that is not in its desired state is a change |
| Apply | `Invoke-DscRunner -Mode Set`, in one PowerShell. A Set tests every resource itself and sets the ones that drifted; its result says which had drifted (`Drifted`), and those are the apply's changes. A resource the runner never tested, such as one a condition skipped, has `Drifted` set to null and is no change. With a DscPipelineRunner older than that, the same PowerShell runs `-Mode Test` first to learn what has drifted |
| `MERIDIAN_PWSH` | the PowerShell to run, when it is not `pwsh` on `PATH` |
| `MERIDIAN_DSC_MODULE` | the module to import: a name PowerShell can find, or the path of its manifest. `Dsc.PipelineRunner` by default |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.dsc`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#dsc).

In `Test` mode the runner records both drift and a resource that could not be
tested as a failure, so a plan lists either as a change, with the runner's
message.

### How long a run takes

Most of a DSC v3 run's time is not the resources: it is starting processes.
A plan or an apply starts one PowerShell to import the runner (about a second
on a hosted Windows agent). The runner then starts one `dsc` per resource
method. Before DscPipelineRunner's
[PR 100](https://github.com/ZanattaMichael/Dsc.PipelineRunner/pull/100), each of
those `dsc` processes also ran dsc's full resource discovery. In dsc 3.3 that
starts yet another PowerShell for the `Microsoft.PowerShell/Discover`
extension, about 2 seconds a call on a hosted Windows agent, and the runner
called `get` after every `test` and `set` as well. A one-resource apply through
DSC v3 took about 25 seconds.

With a runner that has PR 100, the runner looks a type up once and then points
`dsc` at that resource's manifest alone, about 0.08 seconds a call on the same
agent, and it makes no `get` call. Meridian's apply also runs one `Set` instead
of a `Test` and then a `Set`. A one-resource apply is then mostly the one
PowerShell start. Set `PIPELINERUNNER_DSC_FULL_DISCOVERY=1` to turn the
runner's narrowing off.

The runner is tested against a real PowerShell, the DscPipelineRunner module
from the PowerShell Gallery and DSC v3 on Linux, on every pull request that
touches it. On Windows the plugin's own runner is run against both DSC v2 and
DSC v3, and compiled projects that use real Windows resources (`PSDscResources`
through DSC v2, `Microsoft.Windows/Registry` through DSC v3) are run and the
machine's environment and registry are checked afterwards. Reaching another
machine over WinRM or SSH is not tested for real. See
[Real-tool tests](../contributing/real-tool-tests.md#dsc).

### A class-based resource that DSC v2 cannot find

DSC v2 looks a resource up with `Get-DscResource`, and DscPipelineRunner does
the same before it runs anything. A plan that stops with

```text
The term 'MarkerFile' is not recognized as the name of a Resource.
```

for a class-based resource whose module is installed usually means the module's
manifest hides it. When a manifest lists `FunctionsToExport`, `CmdletsToExport`
and `AliasesToExport` all three, without a wildcard, PowerShell does not look
inside the module when it lists it, and reports no DSC resources for it
whatever `DscResourcesToExport` says. Check with:

```pwsh
(Get-Module -ListAvailable TheModule).ExportedDscResources
```

If that is empty, remove one of the three keys from the module's manifest (or
use a module version that has). DSC v3 is not affected: its PowerShell adapter
reads the module itself.
