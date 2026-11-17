# Target: `openvox`

<img src="../branding/logos/meridian-openvox.svg" width="72" alt="Meridian OpenVox plugin logo">

Compiles a ResourceSet into a class and the node statement that assigns it, and
applies it with [OpenVox](https://voxpupuli.org/openvox/), the community-maintained fork
of Puppet: `puppet apply` on the machine itself, or OpenBolt for another machine.

## What it does and does not support

| | |
|---|---|
| Category | configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md), and [below](#resource-reference) for what each becomes) |
| `dependsOn` | Yes, as `require`. OpenVox orders by declared relationships, so the dependency graph survives into the manifest intact |
| `notifies` | Native: `notify`. A service can be restarted (`restart` or `refresh`); an exec can be `run` |
| Not supported | `action: reload`, `start` or `stop` on a service, and notifying a `package`, `file`, `user` or `group`. A refresh restarts a service and takes no argument, and the other types do nothing on one. The compile refuses these and says why; it does not turn a reload into a restart |
| `runtimeWhen` | No. A catalog is compiled before it reaches the node, so there is nothing left to evaluate at apply time. Refused |
| `state: restarted` | No. It is not a state a node can be left in. Notify the service instead |
| References (`${id.param}`) | Yes, settled at compile time to the value the document declares. Anything that cannot be settled is refused. See [References](#references) |
| Plan | Yes: the tool's own `--noop` run |
| Apply | Yes |
| Outputs | No. OpenVox has nothing to hand to a later stage |
| Compiles to | `manifests/init.pp`, `site.pp` |
| Plugin binary | `meridian-target-openvox` |

Meridian never emits a manifest that quietly does less than the document asked for.
Everything this target cannot do is a compile-time refusal that names the resource and
the reason, and every one is listed under [Refusals](#refusals).

One behaviour to know about: an `exec` runs on every apply unless it has `creates`.
Notifying it with `run` makes it run once more when the notifier changes; it does not
make it run only then.

## How it relates to the Puppet target

OpenVox reads the language Puppet reads and ships the same `puppet` command, so the two
targets share one compiler. What `openvox` compiles is byte for byte what `puppet`
compiles, and a test in this repository holds them to that. Switching a document between
the two is a one-word change to `spec.target`.

The difference is in which product runs the manifest:

| | `puppet` | `openvox` |
|---|---|---|
| Tool it expects | Puppet | OpenVox |
| Finds the tool | on `PATH` | on `PATH`, then in `/opt/puppetlabs/bin` where the packages install it |
| Remote transport | Bolt | OpenBolt |
| Settings | `MERIDIAN_PUPPET…` | `MERIDIAN_OPENVOX…` |
| Checks which product it is running | no | yes, see [Making sure it is OpenVox](#making-sure-it-is-openvox) |

Because the settings are separate, one machine can drive both.

## Install

You need OpenVox on the machine that applies the manifest.

- To apply on the machine Meridian runs on, install OpenVox there. The `openvox-agent`
  package from the OpenVox project is the usual way. `gem install openvox` also works and
  needs no root, which is how this repository's CI does it.
- To apply to another host, install OpenBolt on the machine Meridian runs on, and have an
  OpenVox agent package on the node. The catalog is compiled locally and applied by the
  node's agent over SSH or WinRM.

Then put `meridian-target-openvox` beside the `meridian` binary, or in a directory named
by `--plugin-dir` or `MERIDIAN_PLUGIN_DIR`, and check that Meridian sees it:

```
$ meridian plugins
openvox
  category:  configuration-management
  can:       compile, plan, apply
  resources: exec, file, group, package, service, user
```

## Try it on your own machine

[`examples/openvox/local.yaml`](../../examples/openvox/local.yaml) manages two files and a
command under `/tmp`, so it needs no root:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: hello
spec:
  target: openvox
  targetHost:
    from: inline
    host: localhost
  resources:
    - id: hello_dir
      type: file
      state: directory
      params:
        path: /tmp/meridian-hello
    - id: hello_conf
      type: file
      params:
        path: /tmp/meridian-hello/hello.conf
        content: "greeting = hello\n"
        mode: "0644"
      dependsOn:
        - resource: hello_dir
    - id: hello_stamp
      type: exec
      params:
        command: /bin/sh -c 'date > /tmp/meridian-hello/first-applied'
        creates: /tmp/meridian-hello/first-applied
      dependsOn:
        - resource: hello_conf
```

Ask what applying it would change. Nothing is touched:

```
$ meridian plan examples/openvox/local.yaml
  hello | Notice: /Stage[main]/Meridian_hello/File[hello_dir]/ensure: created (noop)
  hello | Notice: /Stage[main]/Meridian_hello/File[hello_conf]/ensure: defined content as '{sha256}6783…' (noop)
  hello | Notice: /Stage[main]/Meridian_hello/Exec[hello_stamp]/returns: executed successfully (noop)
hello (ResourceSet, target openvox on localhost)
  + create hello_dir: ensure: created (noop)
  + create hello_conf: ensure: defined content as '{sha256}6783…' (noop)
  ~ update hello_stamp: returns: executed successfully (noop)
  3 changes to make
```

The lines with a bar are OpenVox's own output, relayed as it runs. The summary below
them is Meridian's, with each change traced back to the `id` you wrote.

Apply it, then plan again:

```
$ meridian apply examples/openvox/local.yaml
hello (ResourceSet, target openvox on localhost)
  + create hello_dir: ensure: created
  + create hello_conf: ensure: defined content as '{sha256}6783…'
  ~ update hello_stamp: returns: executed successfully
  applied 3 changes

$ meridian plan examples/openvox/local.yaml
hello (ResourceSet, target openvox on localhost)
  in sync, nothing to change
```

## A fuller example

[`examples/openvox/web.yaml`](../../examples/openvox/web.yaml) sets up a small web
application on another host: a system user and group, a directory they own, nginx, a
site configuration that restarts nginx when it changes, and a command that runs once.

The part worth reading is how ordering and notification are written:

```yaml
    - id: site_conf
      type: file
      params:
        path: /etc/nginx/conf.d/app.conf
        mode: "0644"
        content: |
          server {
            listen 80;
            root /srv/app/public;
          }
      dependsOn:
        - resource: nginx_pkg
      notifies:
        - resource: nginx_svc
          action: restart
    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
      dependsOn:
        - resource: nginx_pkg
```

`meridian compile examples/openvox/web.yaml --out build` writes
`build/web/manifests/init.pp`, where those two resources become:

```puppet
  file { 'site_conf':
    ensure  => file,
    content => "server {\n  listen 80;\n  root /srv/app/public;\n}\n",
    mode    => '0644',
    path    => '/etc/nginx/conf.d/app.conf',
    require => Package['nginx_pkg'],
    notify  => Service['nginx_svc'],
  }

  service { 'nginx_svc':
    ensure  => running,
    enable  => true,
    name    => 'nginx',
    require => Package['nginx_pkg'],
  }
```

and `build/web/site.pp`, which assigns the class to the host:

```puppet
node 'web01.example.com' {
  include meridian_web
}
```

The compiled files are ordinary manifests. You can read them, review them in a pull
request, or hand `manifests/init.pp` to an OpenVox server as a module's class without
Meridian being involved in the run at all.

## What a document compiles to

`manifests/init.pp` holds a class named `meridian_<document name>` with one resource
declaration for each resource, titled by its `id`. `site.pp` holds the node statement
that includes the class on the document's host. They are separate so a site that
classifies nodes some other way can use the class alone.

- **The class name** comes from the document name, so the name may hold only lower-case
  letters, digits and underscores. A name that cannot be a class name is refused, not
  rewritten.
- **Titles** are the resource ids, which is how a line of the tool's output is traced
  back to your document. The thing being managed is always spelled out in its own
  attribute (`name`, `path` or `command`), never taken from the title.
- **Attributes** are written in alphabetical order with `ensure` first and `require` and
  `notify` last, so the same document always compiles to the same bytes.
- **Values.** A string is single-quoted, so nothing in it is interpolated on the node. A
  string holding a single quote, a backslash, a line break or a tab is double-quoted
  with `$` escaped, to the same effect. Numbers and booleans are written bare, a whole
  number without a decimal point. A list becomes an array and a map becomes a hash. An
  empty (`null`) value is refused.

### Ordering

Every `dependsOn` becomes a `require` on the dependent resource:

```yaml
    - id: app_dir
      type: file
      state: directory
      params: {path: /srv/app}
      dependsOn:
        - resource: app_user
```

```puppet
  file { 'app_dir':
    ensure  => directory,
    path    => '/srv/app',
    require => User['app_user'],
  }
```

The order of declarations in the file means nothing to OpenVox; the `require` is what
orders them. A cycle is refused by Meridian before the plugin is reached.

### Notifications

A `notifies` entry becomes `notify` on the notifier. It also orders the two, so when a
resource both depends on and is notified by another, only the `notify` is written.

| Notified type | Actions accepted | What OpenVox does |
|---|---|---|
| `service` | `restart`, `refresh` (the same thing) | restarts the service, only on a run where the notifier changed |
| `exec` | `run` | runs the command once more on a run where the notifier changed |
| `package`, `file`, `user`, `group` | none | these types do nothing on a refresh, so notifying one is refused |

`reload`, `start` and `stop` on a service are refused, each with its reason. To reload
instead of restart, manage that service with a module of your own that sets its `restart`
command.

### References

A manifest has no way to read one resource's attribute from another at apply time, so a
reference is settled when the document is compiled. `${id.param}` becomes the value the
document gives that param on the resource `id`:

```yaml
    - id: site_root
      type: file
      state: directory
      params:
        path: /srv/app
    - id: index
      type: file
      params:
        path: ${site_root.path}/index.html
        content: hello
```

```puppet
  file { 'index':
    ensure  => file,
    content => 'hello',
    path    => '/srv/app/index.html',
  }
```

- A reference that is the whole value of a param keeps the type of what it reads, so
  `groups: ${app_user.groups}` is a list. Inside longer text it must read a single value.
- References chain: a param that was itself written with a reference can be read.
- A reference does not order the two resources. Use `dependsOn` for that.
- Write a literal `${` by doubling the dollar sign, as a shell script in a file's
  `content` may need. A lone `$` is left alone, and nothing is interpolated on the node.
- A reference that cannot be settled is refused with the rule `invalid-reference`: a
  resource the document does not declare, a param that resource does not set, a bare
  `${id}`, a path deeper than `${id.param}`, an expression, a resource that refers back
  to itself, or a `${` that is never closed.

The same rules hold on every target that shares these six types; see
[the reference](../reference/resource-types.md#references).

### Outputs

None. A ResourceSet has no `outputs`, and the plugin does not offer the outputs
operation, so nothing from an OpenVox run can feed a later stage.

## Resource reference

Anything not listed is refused at compile time, naming the resource and what is
accepted. A resource with no `state` gets the default.

### package

| In the document | In the manifest |
|---|---|
| `state: present` (default) | `ensure => present` |
| `state: latest` | `ensure => latest` |
| `state: absent` | `ensure => absent` |
| `name` (required) | `name` |

Cannot be notified.

### service

| In the document | In the manifest |
|---|---|
| `state: running` (default), `state: started` | `ensure => running` |
| `state: stopped` | `ensure => stopped` |
| `name` (required) | `name` |
| `enabled` | `enable` |

Can be notified with `restart` or `refresh`.

### file

| In the document | In the manifest |
|---|---|
| `state: present` (default), `state: file` | `ensure => file` |
| `state: directory` | `ensure => directory` |
| `state: absent` | `ensure => absent` |
| `path` (required) | `path` |
| `content` | `content` |
| `owner` | `owner` |
| `group` | `group` |
| `mode` | `mode` |

Quote `mode` (`"0644"`) so YAML does not read it as a number. Cannot be notified.

### user

| In the document | In the manifest |
|---|---|
| `state: present` (default) | `ensure => present` |
| `state: absent` | `ensure => absent` |
| `name` (required) | `name` |
| `uid` | `uid` |
| `groups` | `groups` |
| `home` | `home` |
| `shell` | `shell` |
| `system` | `system` |

Cannot be notified.

### group

| In the document | In the manifest |
|---|---|
| `state: present` (default) | `ensure => present` |
| `state: absent` | `ensure => absent` |
| `name` (required) | `name` |
| `gid` | `gid` |
| `system` | `system` |

Cannot be notified.

### exec

| In the document | In the manifest |
|---|---|
| `command` (required) | `command` |
| `creates` | `creates`: the command runs only while this file does not exist |
| `chdir` | `cwd` |
| `shell: true` | `provider => shell`, so the command runs through a shell |

Takes no `state`. Give the command's full path, or set `shell: true`. Can be notified
with `run`.

## Refusals

Each refusal is printed with the file, line and column of the resource, the target, the
resource id and the rule in brackets:

```
site.yaml:37:11: openvox: resource "nginx_conf" [unsupported-action]: notifies resource "nginx_svc" with action "reload", which this target cannot express: …
```

| Rule | When |
|---|---|
| `unsupported-resource-type` | the `type` is not one of the six above |
| `unsupported-state` | a `state` the type does not take, such as `restarted` on a service, or any `state` on an exec |
| `unsupported-param` | a param the type does not take; the message lists the ones it does |
| `missing-param` | a required param is absent: `name`, `path` or `command` |
| `empty-param` | a param is present with no value |
| `unrepresentable-value` | a `null` somewhere inside a value |
| `unsupported-runtime-condition` | the resource has `runtimeWhen` |
| `invalid-reference` | a `${...}` that cannot be settled from the document; see [References](#references) |
| `unsupported-action` | a notify action the notified type cannot perform, or a notified type that cannot be refreshed at all |
| `duplicate-namevar` | two resources of one type manage the same `name`, `path` or `command`. OpenVox would only find this on the node |
| `invalid-class-name` | the document name cannot be a class name |
| `quoted-title`, `multiline-title`, `empty-title` | a resource id that holds a single quote or a line break, or is blank, and so cannot be a title |
| `empty-resource` | a resource that would be declared with no attributes at all |

Five more rules guard the plugin against a malformed tree from Meridian itself and are
not reachable from a valid document: `unknown-dependency`, `unknown-notify-target`,
`dangling-reference`, `duplicate-title` and `ordering-violated`. If you see one, it is a
bug in Meridian; please report it.

## How a run works

Meridian compiles the document, writes the files into its state directory, and runs the
tool there. Which tool depends on the document's host:

- `localhost`, `127.0.0.1` or `::1`: `puppet apply`, on the machine Meridian runs on.
- Anything else: `bolt apply --targets <host>`, which compiles the catalog locally and
  applies it on the node.

A plan is the same run with `--noop`. OpenVox works out every change and makes none.

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine | OpenVox's `puppet` | `puppet apply --detailed-exitcodes --color=false --noop apply.pp` | the same without `--noop` |
| anything else | OpenBolt's `bolt` | `bolt apply apply.pp --targets <host> --format json --noop` | the same without `--noop` |

`apply.pp` is the compiled class followed by the line that includes it. The compiled
`site.pp` does the same through a node statement, which only matches on a machine whose
certificate name is the host, so the runner does not depend on it.

Meridian reads the result from the tool itself. Locally, an exit status of 0 or 2 is
success (2 means changes were made) and 4 or 6 means a resource failed. When an apply
fails part way, Meridian reports the failure with the tool's own explanation, lists what
had already been changed, and exits non-zero. Resources that depended on the failed one
are skipped by OpenVox and are not reported as changed.

Over OpenBolt the changes are read from the report it prints. If that report cannot be
read, the plan is reported as unknown, never as "nothing to change".

### Settings

All of them are environment variables read by the plugin, and all are optional.

| Variable | Meaning | Default |
|---|---|---|
| `MERIDIAN_OPENVOX` | The OpenVox `puppet` program | `puppet` on `PATH`, then `/opt/puppetlabs/bin/puppet` |
| `MERIDIAN_OPENBOLT` | The OpenBolt `bolt` program | `bolt` on `PATH`, then `/opt/puppetlabs/bin/bolt`, then `/opt/puppetlabs/bolt/bin/bolt` |
| `MERIDIAN_OPENVOX_TRANSPORT` | `local` or `bolt`, to override the choice made from the host | chosen from the host |
| `MERIDIAN_OPENVOX_ARGS` | Extra arguments for every run, split on spaces | none |
| `MERIDIAN_OPENVOX_VERIFY` | `warn`, `strict` or `off`, see below | `warn` |

For example, to apply as root on the node and against a particular environment:

```
MERIDIAN_OPENVOX_ARGS="--run-as root" meridian apply site.yaml              # over OpenBolt
MERIDIAN_OPENVOX_ARGS="--environment staging" meridian apply site.yaml      # locally
```

`MERIDIAN_OPENVOX_TRANSPORT=local` is for running Meridian on the node itself when the
document names that node by its real host name.

### Making sure it is OpenVox

Puppet and OpenVox both install a command called `puppet`, so "is `puppet` installed"
does not tell you which one you have. Before a local run the plugin asks the tool for its
`implementation` fact, which OpenVox reports as `openvox`.

| `MERIDIAN_OPENVOX_VERIFY` | If the tool is not OpenVox |
|---|---|
| `warn` (default) | Says so in the run's output, and runs it anyway. The manifest means the same to both. |
| `strict` | Refuses before anything is written or applied. Use it where running the other product is a licensing or policy problem. |
| `off` | Does not ask. Saves under a second per session. |

The check covers local runs. Over OpenBolt the catalog is applied by whatever agent is
installed on the node, and the plugin does not connect to the node just to ask.

## Troubleshooting

**`could not run puppet: it is not installed, or not on the PATH`**
OpenVox is not installed where Meridian runs, or is somewhere the plugin does not look.
Set `MERIDIAN_OPENVOX` to the full path of its `puppet` command.

**`… is not OpenVox: it has no implementation fact`**
The `puppet` that was found is Puppet, or an OpenVox release from before the fact
existed. Point `MERIDIAN_OPENVOX` at OpenVox's command, or see the table above.

**`puppet apply reported a failed resource`**
OpenVox ran and a resource failed. The tool's own error follows the message. Fix the
cause on the node or in the document and apply again; what already succeeded will not be
redone.

**`bolt apply failed`**
Usually the node could not be reached, or has no agent installed. The output that follows
is OpenBolt's own.

**A plan always shows an `exec` as a change**
It has no `creates`, so it runs on every apply. That is what the document says; add
`creates` if the command should run once.

## How this target is tested

- Golden files: every shared fixture and both documented examples are compiled and
  compared with recorded output in `plugins/openvox/testdata`, and with what the Puppet
  target emits.
- The runner is tested against stand-ins for `puppet` and `bolt`, so it runs anywhere.
- The plugin binary is started as a real process and driven over the plugin protocol.
- In CI the same plugin is run against a real OpenVox: every compiled manifest is
  parsed by it, and manifests are planned, applied, changed, drifted and removed on disk.

To run the last group yourself:

```
gem install openvox
MERIDIAN_TEST_REAL_OPENVOX=1 cargo test -p meridian-target-openvox --test real
```
