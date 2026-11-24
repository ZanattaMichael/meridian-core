# Target: `openvox`

<img src="../branding/logos/meridian-openvox.svg" width="72" alt="Meridian OpenVox plugin logo">

Compiles a ResourceSet into a class and the node statement that assigns it, and
applies it with [OpenVox](https://voxpupuli.org/openvox/), the community-maintained fork
of Puppet: `puppet apply` on the machine itself, or OpenBolt for another machine.

## What it does and does not support

| | |
|---|---|
| Category | configuration management |
| Resource types | `package`, `packageRepository`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md), and [below](#resource-reference) for what each becomes), and any Puppet type beside them: `cron`, `mysql::db`, `native/file` ([below](#puppet-types-beside-the-six)) |
| `dependsOn` | Yes, as `require`. OpenVox orders by declared relationships, so the dependency graph survives into the manifest intact |
| `notifies` | Native: `notify`. A service can be restarted (`restart` or `refresh`); an exec can be `run`; a Puppet type of its own can be refreshed (`refresh`) |
| Not supported | `action: reload`, `start` or `stop` on a service, and notifying a `package`, `file`, `user` or `group`. A refresh restarts a service and takes no argument, and the other types do nothing on one. The compile refuses these and says why; it does not turn a reload into a restart |
| `runtimeWhen` | No. A catalog is compiled before it reaches the node, so there is nothing left to evaluate at apply time. Refused |
| `adopt` | Accepted, and changes nothing: OpenVox keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
| `state: restarted` | No. It is not a state a node can be left in. Notify the service instead |
| Secrets | Yes: by Hiera, as a `Sensitive` lookup, exactly as the Puppet target does. See [Secrets](#secrets) |
| Editor schemas | Yes: `meridian schema sync` asks `puppet describe` what each type takes, as the Puppet target does. See [Editor schemas](#editor-schemas) |
| References (`${id.param}`) | Yes, settled at compile time to the value the document declares. Anything that cannot be settled is refused. See [References](#references) |
| Plan | Yes: the tool's own `--noop` run |
| Apply | Yes |
| Outputs | No. OpenVox has nothing to hand to a later stage |
| Compiles to | `manifests/init.pp`, `site.pp` |
| Plugin binary | `meridian-target-openvox` |

What the target cannot do is [refused](#refusals). An `exec` runs on every apply unless it
has `creates`; `run` notifies make it run once more.

## How it relates to the Puppet target

The two targets share one compiler and emit identical bytes (tested). They differ in what
runs the manifest:

| | `puppet` | `openvox` |
|---|---|---|
| Tool it expects | Puppet | OpenVox |
| Finds the tool | on `PATH` | on `PATH`, then in `/opt/puppetlabs/bin` where the packages install it |
| Remote transport | Bolt | OpenBolt |
| Settings | `MERIDIAN_PUPPET…` | `MERIDIAN_OPENVOX…` |
| Checks which product it is running | no | yes, see [Making sure it is OpenVox](#making-sure-it-is-openvox) |

## Install

- Locally: install `openvox-agent`, or `gem install openvox` (no root; used by CI).
- Remote: OpenBolt locally and an OpenVox agent on the node.

Put `meridian-target-openvox` beside `meridian`, or in `--plugin-dir` or
`MERIDIAN_PLUGIN_DIR`:

```
$ meridian plugins
openvox
  category:  configuration-management
  can:       compile, plan, apply
  resources: exec, file, group, package, service, user
  plan file: guarded
```

## Try it on your own machine

[`examples/openvox/local.yaml`](../../examples/openvox/local.yaml) works under `/tmp`, no
root:

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

Plan:

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

Barred lines are OpenVox's output; the summary is Meridian's. Apply, then plan again:

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

[`examples/openvox/web.yaml`](../../examples/openvox/web.yaml): a user and group, a
directory, nginx, a site config that restarts nginx, and a one-off command.

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

`manifests/init.pp` can also be used as a module class on an OpenVox server.

## What a document compiles to

`manifests/init.pp` holds class `meridian_<document name>`, one declaration per resource
titled by its `id`. `site.pp` holds the node statement.

- **Class name**: lower-case letters, digits and underscores; otherwise refused.
- **Titles** are ids; the managed thing is in `name`, `path` or `command`.
- **Attributes** are sorted, `ensure` first, `require` and `notify` last.
- **Values**: strings single-quoted (double-quoted with `$` escaped when needed), numbers and
  booleans bare, lists as arrays, maps as hashes. `null` is refused.

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

### Notifications

`notifies` becomes `notify`, which also orders; a matching `require` is dropped.

| Notified type | Actions accepted | What OpenVox does |
|---|---|---|
| `service` | `restart`, `refresh` (the same thing) | restarts the service, only on a run where the notifier changed |
| `exec` | `run` | runs the command once more on a run where the notifier changed |
| `package`, `file`, `user`, `group` | none | these types do nothing on a refresh, so notifying one is refused |

`reload`, `start` and `stop` are refused. For a reload, set the service's `restart`
command in your own module.

### References

`${id.param}` is settled at compile to that param's value on resource `id`:

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

- A whole-value reference keeps its type; inside text it must be a single value.
- References chain. They do not order resources.
- `$${` is a literal `${`.
- Anything unsettleable is refused (`invalid-reference`).

See [the reference](../reference/resource-types.md#references).

### Outputs

None.

## Resource reference

Other types are [Puppet types](#puppet-types-beside-the-six). Unknown params are refused.
No `state` means the default.

### package

| In the document | In the manifest |
|---|---|
| `state: present` (default) | `ensure => present` |
| `state: latest` | `ensure => latest` |
| `state: absent` | `ensure => absent` |
| `name` (required) | `name` |

Cannot be notified.

### packageRepository

| In the document | In the manifest |
|---|---|
| `format: yum` | `yumrepo` |
| `format: apt` | refused: no built-in apt source; use `apt::source` |
| `state: present` (default), `state: absent` | `ensure => present`, `ensure => absent` |
| `url`, `key` | `baseurl`, `gpgkey` (and `gpgcheck => true`) |

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
| `state: link` | `ensure => link`, with `target` |
| `state: absent` | `ensure => absent` |
| `path` (required) | `path` |
| `content` | `content` |
| `owner` | `owner` |
| `group` | `group` |
| `mode` | `mode` |

Quote `mode` (`"0644"`). Cannot be notified.

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

### Puppet types beside the six

As on the [Puppet target](puppet.md#puppet-types-beside-the-six): any loadable Puppet type
(`cron`, `mysql::db`), params unchecked, `state` as `ensure`, `refresh` notifies, and
`native/file` for Puppet's own `file`.
[`examples/puppet/native-types.yaml`](../../examples/puppet/native-types.yaml) works with
`spec.target: openvox`. Such documents compile for Puppet and OpenVox only; the
[`native-type`](../reference/lint.md#native-type) lint marks them.

## Refusals

Each refusal names file, line, column, target, id and rule:

```
site.yaml:37:11: openvox: resource "nginx_conf" [unsupported-action]: notifies resource "nginx_svc" with action "reload", which this target cannot express: …
```

| Rule | When |
|---|---|
| `unsupported-resource-type` | the `type` is not one of the six above and not a Puppet type name either: a wrong case, a dotted name, or `native/` on a name no shared type has taken |
| `unsupported-state` | a `state` the type does not take, such as `restarted` on a service, or any `state` on an exec |
| `unsupported-param` | a param the type does not take; the message lists the ones it does |
| `missing-param` | a required param is absent: `name`, `path` or `command` |
| `empty-param` | a param is present with no value |
| `invalid-param` | a `null` somewhere inside a value |
| `unsupported-runtime-condition` | the resource has `runtimeWhen` |
| `invalid-reference` | a `${...}` that cannot be settled from the document; see [References](#references) |
| `unsupported-action` | a notify action the notified type cannot perform, a notified type that cannot be refreshed at all, or anything but `refresh` on a Puppet type of its own |
| `duplicate-resource` | two resources of one type manage the same `name`, `path` or `command`. OpenVox would only find this on the node |
| `invalid-name` | the document name cannot be a class name |
| `invalid-name` | a resource id that holds a single quote or a line break, or is blank, and so cannot be a title |
| `missing-param` | a resource of a shared type that would be declared with no attributes at all |
| `param-conflicts-with-state` | `content` on a `directory`; `state` and `params.ensure` both on a Puppet type of its own |

`unknown-dependency`, `unknown-notify-target`, `invalid-reference`, `duplicate-resource`
and `ordering-violated` are internal guards; report them if seen.

## How a run works

- `localhost`, `127.0.0.1`, `::1`: `puppet apply`.
- Otherwise: `bolt apply --targets <host>`.

A plan adds `--noop`.

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine | OpenVox's `puppet` | `puppet apply --detailed-exitcodes --color=false --noop apply.pp` | the same without `--noop` |
| anything else | OpenBolt's `bolt` | `bolt apply apply.pp --targets <host> --format json --noop` | the same without `--noop` |

`apply.pp` is the class plus an include, so the run does not depend on the node's
certificate name.

Locally, exit 0 or 2 is success (2: changed); 4 or 6 is a failed resource, reported with
what had changed. Over OpenBolt an unreadable report makes the plan unknown.

A failed resource is read as on [Puppet](puppet.md#reading-a-failure): a
[resource failure](../reference/state.md#resource-failures) per resource from OpenVox's `Error:` and `Skipping because of
failed dependencies` lines, or from OpenBolt's JSON report.

### Settings

Optional environment variables.

| Variable | Meaning | Default |
|---|---|---|
| `MERIDIAN_OPENVOX` | The OpenVox `puppet` program | `puppet` on `PATH`, then `/opt/puppetlabs/bin/puppet` |
| `MERIDIAN_OPENBOLT` | The OpenBolt `bolt` program | `bolt` on `PATH`, then `/opt/puppetlabs/bin/bolt`, then `/opt/puppetlabs/bolt/bin/bolt` |
| `MERIDIAN_OPENVOX_TRANSPORT` | `local` or `bolt`, to override the choice made from the host | chosen from the host |
| `MERIDIAN_OPENVOX_ARGS` | Extra arguments for every run, split like a shell command line (quotes hold a word together) | none |
| `MERIDIAN_OPENVOX_VERIFY` | `warn`, `strict` or `off`, see below | `warn` |
| `MERIDIAN_SSH_AGENT` | The `ssh-agent` program, started for a credential's key; see [Credentials](#credentials) | `ssh-agent` on `PATH` |
| `MERIDIAN_SSH_ADD` | The `ssh-add` program | `ssh-add` on `PATH` |

Settings can also go in `meridian.yaml` under `spec.settings.openvox`; the environment
wins. See the [plugin settings reference](../reference/plugin-settings.md#openvox).
Applying as root against an environment:

```
MERIDIAN_OPENVOX_ARGS="--run-as root" meridian apply site.yaml              # over OpenBolt
MERIDIAN_OPENVOX_ARGS="--environment staging" meridian apply site.yaml      # locally
```

`MERIDIAN_OPENVOX_TRANSPORT=local` runs on this node when the document names it by host
name.

### Secrets

A [secret](../reference/dsl/secrets.md) compiles to a Hiera lookup in `Sensitive`, redacted
in reports and diffs:

```puppet
    content => Sensitive(lookup('meridian_secret_db_password')),
```

The value goes into a private Hiera data file for the run, passed by
`--hiera_config` (OpenBolt: `--hiera-config`). A secret in a list or map is refused. Same as
[Puppet](puppet.md#secrets); tested with `MERIDIAN_TEST_REAL_OPENVOX=1`.

### Making sure it is OpenVox

Both install `puppet`. Before a local run the plugin checks the `implementation` fact is
`openvox`.

| `MERIDIAN_OPENVOX_VERIFY` | If the tool is not OpenVox |
|---|---|
| `warn` (default) | Says so in the run's output, and runs it anyway. The manifest means the same to both. |
| `strict` | Refuses before anything is written or applied. Use it where running the other product is a licensing or policy problem. |
| `off` | Does not ask. Saves under a second per session. |

Over OpenBolt the node's agent is not checked.

## Editor schemas

As on [Puppet](puppet.md#editor-schemas), using `MERIDIAN_OPENVOX` and
`MERIDIAN_OPENVOX_ARGS`, pinned as `openvox`.

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md). `bolt`
reads it from an inventory written for the run:

```yaml
spec:
  target: openvox
  targetHost:
    from: inline
    host: web01.example.com
    credential: deploy
  secrets:
    - name: deploy_key
      from: env
      key: DEPLOY_KEY
  credentials:
    - name: deploy
      type: ssh
      user: deploy
      privateKey: ${{ secret.deploy_key }}
```

- **A user and password** (`ssh` with `password`, or `usernamePassword`) go into a private
  inventory, removed after the run, passed by `--inventoryfile`:

  ```yaml
  targets:
    - uri: "web01.example.com"
      config:
        transport: ssh
        ssh:
          user: "deploy"
          password: "..."
  ```

- Leave `--inventoryfile` out of `MERIDIAN_OPENVOX_ARGS` when a credential is named.
- **A key** goes into an SSH agent Meridian starts for the stage, loaded through `ssh-add`'s
  stdin; `bolt` gets only `SSH_AUTH_SOCK`. A `passphrase` is answered from a private file.
  See [0028](../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
- **`agent: true`** uses your own agent. **`from: ambient`** leaves signing in to the tool.
- **Refused at compile** (`unsupported-credential`): a `certificate` (load it in your own
  agent, `agent: true`), a `domain`, a user name with a space or line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). Leave the credential out, or name another host.

The plan names the identity (`web (openvox) on web01.example.com as deploy (ssh, user
deploy)`), never a value. `ssh-agent` and `ssh-add` come from `PATH` or `MERIDIAN_SSH_AGENT`
and `MERIDIAN_SSH_ADD`. Tested in the Puppet plugin against stand-ins; real `bolt` is not
run in CI.

## Troubleshooting

**`could not run puppet: it is not installed, or not on the PATH`**
Set `MERIDIAN_OPENVOX` to the full path of OpenVox's `puppet`.

**`… is not OpenVox: it has no implementation fact`**
It found Puppet or an old OpenVox. Point `MERIDIAN_OPENVOX` at OpenVox.

**`puppet apply reported a failed resource`**
The tool's error follows. Fix it and apply again.

**`bolt apply failed`**
Usually the node is unreachable or has no agent. OpenBolt's output follows.

**A plan always shows an `exec` as a change**
Add `creates` to run it once.

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--evaltrace`, `--debug`, or `--debug --trace` to `puppet apply`, and `--log-level info`, `debug` or `trace` to `bolt apply`, as the Puppet target does.

## How this target is tested

- Golden files in `plugins/openvox/testdata`, compared with Puppet's output.
- Stand-ins for `puppet` and `bolt`; the binary over the plugin protocol.
- CI parses every manifest with real OpenVox, and plans, applies, changes and removes.

To run the real-tool tests:

```
gem install openvox
MERIDIAN_TEST_REAL_OPENVOX=1 cargo test -p meridian-target-openvox --test real
```

## Inspecting one resource

[`meridian inspect`](../reference/cli.md#inspect) works as for [Puppet](puppet.md#inspecting-one-resource),
with OpenVox's `puppet` and OpenBolt's `bolt`, after the same check that the tool is OpenVox.
