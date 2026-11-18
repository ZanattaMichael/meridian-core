# Target: `dsc`

<img src="../branding/logos/meridian-dsc.svg" width="72" alt="Meridian DSC plugin logo">

Compiles a ResourceSet into a DscPipelineRunner configuration and runs it with
`Invoke-DscRunner`.

It covers DSC v2 (`Invoke-DscResource`) and DSC v3 (the `dsc` command); the engine is a
setting.

## What it does and does not support

| | |
|---|---|
| Resource types | DSC's own: `Module/Resource`, such as `PSDscResources/Service` or `Microsoft.Windows/Registry`. The abstract types (`package`, `service`, ...) are not taken |
| Settings | `spec.settings`: `engine`, `target` and `credential`, [how the configuration is run](#how-the-configuration-is-run) |
| `dependsOn` | Yes, as the runner's own `dependsOn` |
| `notifies` | Native: the runner's `notify`. The action is `refresh`: when the notifier really changed, the notified resource's `Set` runs even if its `Test` says there is nothing to do |
| `runtimeWhen` | Yes: a `preCondition` |
| `state` | No. A DSC resource carries its desired state in its own properties (`Ensure`, `_exist`), so set it in `params` |
| Plan | Yes: the runner's `Test` mode |
| Outputs | No |

## What a document looks like

`type` is `Module/Resource` for v2 and v3 alike; `params` are its properties. The engine
checks the type.

### How the configuration is run

Set under [`spec.settings`](../reference/dsl/index.md#settings), as text, using the
runner's `PipelineRunnerSettings` keys:

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

Other values are refused (`invalid-setting`), as are other keys
(`the document sets spec.settings.transport, and target "dsc" takes only: credential,
engine, target`). Interpolation works: `target: "${{ var.transport }}"`.

The old `runner` resource still compiles with a warning until the next `apiVersion`.
Conflicting values are refused (`conflicting-settings`), and so is `runtimeWhen` on it
(`unsupported-runtime-condition`).

### The version of DSC

[`targetVersion`](../reference/dsl/index.md#targetversion) `2` or `3` chooses the engine:

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

A `targetVersion` that disagrees with `spec.settings.engine` (including `Auto`) is refused
(`conflicting-settings`). Other versions are refused.

For another host, `spec.settings.target` (WinRM or SSH) is required (`missing-param`).

## References

`${id.Property}` reads another resource's reported property after it runs, through the
runner's `reference()` accessor, and orders the reader after it. `$` and backticks in
params are escaped.

## Conditions

`runtimeWhen` is a `preCondition`, a PowerShell predicate run just before the resource.

```yaml
runtimeWhen: "equals (nodeName) 'web'"
```

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

The Datum project has one layer and one node; Meridian has already resolved the data.

## Importing

`meridian import` writes one ResourceSet per node from three formats
([guide](../operations/importing.md)).

| Source | Recognised by | Document |
|---|---|---|
| A PowerShell `Configuration` block | `*.ps1` | `targetVersion: "2"`, one document for each `Node` block |
| A DSC v3 configuration document, YAML or JSON | `*.dsc.yaml`, `*.dsc.yml`, `*.dsc.json`, `*.dsc.config.*`; another name needs `--from dsc` | `targetVersion: "3"` |
| A DscPipelineRunner project, which is what this target compiles to | `Datum.yml` and the node files beside it; import the directory with `-r --from dsc` | the engine and target of the project's `PipelineRunnerSettings` |

An instance becomes a `Module/Resource` resource. A name such as `Launch key` becomes
`launch_key`, with a note.

### A PowerShell configuration

The script is not run. Read: `Configuration`, `Import-DscResource`, `Node`, and resource
blocks with literal properties.

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

`resources` import as is; `[resourceId(...)]` becomes `dependsOn`; the host is `localhost`.
`[[text]` is the literal `[text]`. DSC expressions, `parameters`, `variables` and
`metadata` are kept as text and marked.

### A runner project

The target's output read back: `dependsOn`, `notify` (as `refresh`), `preCondition` (as
`runtimeWhen`) and `$((reference 'id').Path)` (as `${id.Path}`) return, and escaping is
removed. `target` becomes the host and `spec.settings`. Other PowerShell expressions are
marked. It round-trips byte for byte.

Tests: [`plugins/dsc/tests/import.rs`](../../plugins/dsc/tests/import.rs), with
[`iis/WebServer.ps1`](../../plugins/dsc/testdata/import/iis/WebServer.ps1),
[`iis.expected.node.yml`](../../plugins/dsc/testdata/import/iis.expected.node.yml),
[`iis.meridian.yaml`](../../plugins/dsc/testdata/import/iis.meridian.yaml),
[`machine.dsc.yaml`](../../plugins/dsc/testdata/import/machine.dsc.yaml) and
[`fleet.meridian.yaml`](../../plugins/dsc/testdata/import/fleet.meridian.yaml).

## Editor schemas

`meridian schema sync` asks DSC for each resource's properties
([Tool schemas](../tooling/schema-sync.md)):

- PowerShell modules: `Get-DscResource -Module` (in `MERIDIAN_PWSH`), or the module's
  `[DscResource()]` classes when that lists nothing.
- `dsc` (or `MERIDIAN_DSC`): `dsc resource list` and `dsc resource schema`.

| | |
|---|---|
| Described in full | every resource of each PowerShell module the documents use, from its property types (`[string]`, `[Int32]`, `[string[]]`, `[PSCredential]`), which are mandatory, and the values an enum property allows; and each DSC v3 resource the documents use, from the JSON Schema its manifest declares |
| Listed by name | every other DSC v3 resource `dsc resource list` reports |
| Pinned | PowerShell's version, each module's version, each DSC v3 resource's version |

Missing modules, undescribed types and PowerShell errors are noted.

## Running

| | |
|---|---|
| Plan | `Invoke-DscRunner -Mode Test`: every resource is tested, none is set. A resource that is not in its desired state is a change |
| Apply | `Invoke-DscRunner -Mode Set`, in one PowerShell. A Set tests every resource itself and sets the ones that drifted; its result says which had drifted (`Drifted`), and those are the apply's changes. A resource the runner never tested, such as one a condition skipped, has `Drifted` set to null and is no change. With a DscPipelineRunner older than that, the same PowerShell runs `-Mode Test` first to learn what has drifted |
| `MERIDIAN_PWSH` | the PowerShell to run, when it is not `pwsh` on `PATH` |
| `MERIDIAN_DSC_MODULE` | the module to import: a name PowerShell can find, or the path of its manifest. `Dsc.PipelineRunner` by default |

Settings can also go in `meridian.yaml` under `spec.settings.dsc`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#dsc).

In `Test` mode, drift and untestable resources both show as changes.

### How long a run takes

A DSC v3 run's time is mostly process starts: one PowerShell (about 1 s on a hosted Windows
agent), then one `dsc` per resource method. Before DscPipelineRunner
[PR 100](https://github.com/ZanattaMichael/Dsc.PipelineRunner/pull/100), each `dsc` ran full
discovery (about 2 s per call in dsc 3.3) and the runner called `get` after every `test` and
`set`; a one-resource apply took about 25 s. With PR 100 each call takes about 0.08 s, no
`get` is made, and Meridian runs one `Set` instead of `Test` then `Set`.
`PIPELINERUNNER_DSC_FULL_DISCOVERY=1` turns the narrowing off.

CI runs real PowerShell, DscPipelineRunner and DSC v3 on Linux, and DSC v2 and v3 on
Windows with real resources. WinRM and SSH are not tested for real. See
[Real-tool tests](../contributing/real-tool-tests.md#dsc).

### A class-based resource that DSC v2 cannot find

A plan that stops with

```text
The term 'MarkerFile' is not recognized as the name of a Resource.
```

for an installed class-based resource usually means the manifest lists
`FunctionsToExport`, `CmdletsToExport` and `AliasesToExport` without a wildcard, which hides
its DSC resources. Check with:

```pwsh
(Get-Module -ListAvailable TheModule).ExportedDscResources
```

If empty, remove one of the three keys. DSC v3 is not affected.
