# Target: `puppet`

<img src="../branding/logos/meridian-puppet.svg" width="72" alt="Meridian Puppet plugin logo">

Compiles a ResourceSet into a Puppet class and the node statement that assigns
it, and applies it with `puppet apply` on the node itself, or with Bolt for
another machine. Puppet still does the work: Meridian writes the manifest a
person would have written and runs Puppet on it.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)) |
| `dependsOn` | Yes, as `require`. Puppet orders by declared relationships, so the dependency graph survives into the manifest intact |
| `notifies` | Native: `notify`. A service can be restarted (`restart` or `refresh`); an exec can be `run` |
| `runtimeWhen` | No. A catalog is compiled before it reaches the node, so there is nothing left to evaluate at apply time. Refused |
| Secrets | Yes: by Hiera. The manifest looks each up as a `Sensitive` value, and the run is given a data file and a `hiera.yaml` only the user can read. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: `--noop`, with `--detailed-exitcodes`. Every resource reports whether it would change |
| Apply | Yes |
| Outputs | No. A manifest produces no values for a later stage to read |
| Compiles to | `manifests/init.pp` and `site.pp` |
| Runs with | `puppet apply` on the node itself, `bolt apply` for another host |

What it does not do:

- **`action: reload`, `start` or `stop` on a service are refused.** Puppet's
  refresh restarts a service and takes no argument. The compile says why; it
  does not turn a reload into a restart. See
  [Ordering and notifications](#ordering-and-notifications).
- **No Puppet master, no PuppetDB.** Plan and apply are masterless, and Hiera
  is used for one thing, [handing over the document's secrets](#secrets). The
  compiled class works on a master, but you copy it there yourself.
- **No resource types beyond the six.** There is no passthrough to arbitrary
  Puppet types. Anything else is refused at compile time.
- **`package.version` and `exec.removes` are refused**, though Puppet could
  express them; see [What is refused](#what-is-refused).
- **It creates no infrastructure.** It configures a node that already exists.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resource types](#resource-types) ·
[Ordering and notifications](#ordering-and-notifications) ·
[What a compile writes](#what-a-compile-writes) ·
[Running](#running) ·
[Settings](#settings) ·
[Secrets](#secrets) ·
[What is refused](#what-is-refused) ·
[Troubleshooting](#troubleshooting) ·
[How it is tested](#how-it-is-tested)

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | `puppet` for a local run (the `puppet` gem, or the puppet-agent package); `bolt` for another host |
| A host reached with Bolt | Whatever Bolt needs: SSH or WinRM, and Bolt's own inventory or `--targets` settings through [`MERIDIAN_PUPPET_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Puppet |

Tested with Puppet 8 (the `puppet` gem) and with OpenVox, which the
[`openvox`](openvox.md) target drives through this plugin's compiler.

## A first document

[`examples/puppet/web-server.yaml`](../../examples/puppet/web-server.yaml)
installs nginx, writes its configuration and keeps it running:

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: puppet
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
        content: |
          server {
            listen 80;
            root /srv/www;
          }
        owner: root
        mode: "0644"
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

It compiles to:

```puppet
class meridian_web {
  package { 'nginx_pkg':
    ensure => present,
    name   => 'nginx',
  }

  file { 'nginx_conf':
    ensure  => file,
    content => "server {\n  listen 80;\n  root /srv/www;\n}\n",
    mode    => '0644',
    owner   => 'root',
    path    => '/etc/nginx/conf.d/site.conf',
    require => Package['nginx_pkg'],
    notify  => Service['nginx_svc'],
  }

  service { 'nginx_svc':
    ensure  => running,
    enable  => true,
    name    => 'nginx',
    require => Package['nginx_pkg'],
  }
}
```

The same document compiles for Ansible, Chef, Salt, OpenVox or cloud-init by
changing `target`; the [conformance matrix](../reference/conformance-matrix.md)
says where those targets differ.

## Resource types

The six shared types, as this target takes them. A param not listed is refused
(`unsupported-param`); a state not listed is refused (`unsupported-state`).

| Type | Puppet type | States (`ensure`) | Params | Required |
|---|---|---|---|---|
| `package` | `package` | `present` (default), `absent`, `latest` | `name` | `name` |
| `service` | `service` | `running` (default), `started` (the same), `stopped` | `name`, `enabled` (`enable`) | `name` |
| `file` | `file` | `file` (default), `present` (the same), `directory`, `absent` | `path`, `content`, `owner`, `group`, `mode` | `path` |
| `user` | `user` | `present` (default), `absent` | `name`, `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `group` | `present` (default), `absent` | `name`, `gid`, `system` | `name` |
| `exec` | `exec` | none; an exec has no state | `command`, `chdir` (`cwd`), `creates`, `shell` | `command` |

Notes on how they map:

- A resource's **id is the Puppet title**; the Puppet name or path comes from
  `name` or `path`. Puppet identifies a managed thing by that name, not by the
  title, so two resources that manage the same package, file, service, user
  or group are refused at compile time (`duplicate-resource`), where Puppet
  would only have refused them on the node.
- `file` with `state: directory` and `content` is refused
  (`param-conflicts-with-state`): a directory has no content.
- `exec` with `shell: true` runs through the shell provider, so a pipe or a
  redirection works. Without it the command is run as written, so a pipe
  reaches the program as an argument.
- `exec` runs every time Puppet does unless `creates` says when it is already
  done. Puppet's `unless` and `onlyif` are not exposed: Meridian has
  `runtimeWhen` for that, and this target refuses it.
- A `mode` is written as Puppet reads it: quote it in the document
  (`"0644"`), or the YAML reads it as a number and the compile refuses it.

## Ordering and notifications

`dependsOn` becomes `require`. The resources are also written in apply order,
so a reader of the manifest sees the same order Meridian decided; nothing is
flattened into "top to bottom".

`notifies` becomes `notify`, and Puppet refreshes the notified resource at the
end of the run if the notifier changed:

| Notified type | Actions | What Puppet does |
|---|---|---|
| `service` | `restart`, `refresh` | restarts the service |
| `exec` | `run` | runs the command again, even if `creates` says it already ran |
| anything else | none | refused: `unsupported-action` |

`reload`, `start` and `stop` on a service are refused with the reason: Puppet's
refresh restarts and takes no argument, a service that should be running is
already declared `state: running`, and Puppet has no way to stop a service in
response to a change. To reload instead of restart, give the service a
`restart` attribute in a Puppet module of your own; Meridian will not emit a
restart where a reload was asked for.

## What a compile writes

```
manifests/init.pp
site.pp
```

`manifests/init.pp` holds a class named `meridian_<document name>` with one
resource declaration per resource, titled by its id. `site.pp` holds the node
statement that includes the class on the document's host. They are separate
so a site that classifies nodes some other way can use the class alone.

Because the class name comes from the document name, the name may hold only
lower-case letters, digits and underscores. A name that cannot be a class
name is refused, not rewritten.

Strings are written as Puppet strings: single-quoted where the text has no
escape in it, double-quoted with `\n`, `\"` and `\$` where it has. A value
Meridian placed from `--data`, `--set` or a data layer is written so Puppet
reads it as text and never as an interpolation
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Running

Puppet has no way to push a catalog to a node, so there are two ways to apply
one, chosen by the document's host:

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine (`localhost`, `127.0.0.1`, `::1`) | `puppet` | `puppet apply --detailed-exitcodes --color=false --noop apply.pp` | the same without `--noop` |
| anything else | `bolt` | `bolt apply apply.pp --targets <host> --format json --noop` | the same without `--noop` |

`apply.pp` is the class and the node statement joined, written into the
stage's work directory. Changes are read from Puppet's own report, resource
by resource: a plan lists every resource that would change and touches none
of them; an apply lists what changed. With `--detailed-exitcodes`, status `2`
means changes were made and is a success; `4` and `6` mean a resource failed,
and the stage fails with the end of Puppet's output and the changes it made
before failing.

When Bolt's report cannot be read, the plan is reported as **unknown**, not as
empty, so a run never says "nothing to do" on no evidence.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_PUPPET` | the `puppet` program, when it is not on `PATH` |
| `MERIDIAN_BOLT` | the `bolt` program, when it is not on `PATH` |
| `MERIDIAN_PUPPET_TRANSPORT` | `local` or `bolt`, to override the choice made from the host |
| `MERIDIAN_PUPPET_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--confdir /tmp/p --vardir /tmp/v --codedir /tmp/c` for an isolated run, or Bolt's `--inventoryfile inventory.yaml` |
| `MERIDIAN_SSH_AGENT` | the `ssh-agent` program, when it is not on `PATH`; started for a credential's key, see [Credentials](#credentials) |
| `MERIDIAN_SSH_ADD` | the `ssh-add` program, when it is not on `PATH` |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.puppet`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#puppet).

## Secrets

A [secret](../reference/dsl/secrets.md) in a resource's params reaches Puppet
through Hiera, and through nothing else.

```yaml
spec:
  target: puppet
  secrets:
    - name: db_password
      from: env
      key: APP_DB_PASSWORD
  resources:
    - id: db_conf
      type: file
      params:
        path: /etc/app/db.conf
        mode: "0600"
        content: ${{ secret.db_password }}
```

**What the compile writes.** The param becomes a lookup of the secret's key,
wrapped in `Sensitive`, so that Puppet redacts the value wherever it would
print it: in its report, in a `--noop` run's `current_value`/`should be` line,
and in a file's diff.

```puppet
  file { 'db_conf':
    ensure  => file,
    content => Sensitive(lookup('meridian_secret_db_password')),
    mode    => '0600',
    path    => '/etc/app/db.conf',
  }
```

The compiled manifest holds the key, never the value, so it can be reviewed
and kept. `meridian compile` writes exactly this.

**How the value reaches the tool.** When a stage runs, the runner writes a
Hiera data file, `common.yaml`, with each secret under its key, into a
directory only the user can enter (mode `0700`, the file `0600`, on a memory
filesystem where the system has one), and a `hiera.yaml` beside it that names
that directory as its data directory. The run is given the configuration and
nothing more: `puppet apply ... --hiera_config <dir>/hiera.yaml` on this
machine, `bolt apply ... --hiera-config <dir>/hiera.yaml` for another host.
Over Bolt the catalog is compiled on this machine, so the lookup is answered
here and the value travels to the node inside the catalog, the way Bolt sends
every catalog. Both files are overwritten and removed when the run ends.
Neither the command line nor the tool's environment ever holds the value, and
nothing in the stage's work directory does.

**What is refused.** A secret is a whole param. One inside a list or a map
(`groups: [..., "${{ secret.x }}"]`) is refused at compile, rule
`unsupported-secret-place`, because a lookup is one value and the plugin does
not build a list around it. A document's own Hiera is not read: the
configuration the run is given names Meridian's data file alone, so a
`hiera.yaml` of your own is not consulted during a Meridian run.

The handover is tested against a stand-in `puppet` that keeps a copy of the
data file, its mode, its arguments and its environment, and against the real
`puppet` in CI (`MERIDIAN_TEST_REAL_PUPPET=1`), which applies a file whose
content is a secret and whose value then appears in the file and in nothing
Puppet printed or the plugin wrote.

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md) the host is
reached as. Another machine is reached with `bolt`, which reads how to sign in from an inventory, so the plugin writes one for the run:

```yaml
spec:
  target: puppet
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

- **The user and a password** (an `ssh` credential with a `password`, or a
  `usernamePassword` one) go into a Bolt inventory written for the run, in a directory only
  you can enter and removed when the run ends, and `--inventoryfile` names it:

  ```yaml
  targets:
    - uri: "web01.example.com"
      config:
        transport: ssh
        ssh:
          user: "deploy"
          password: "..."
  ```

- Bolt's own `--inventoryfile` in `MERIDIAN_PUPPET_ARGS` is then a second inventory; leave
  it out when the document names a credential.
- **A key is never on disk.** It goes into an SSH agent Meridian starts for the stage:
  `ssh-agent -D -a <socket>`, its socket in a directory only you can enter, loaded with
  `ssh-add -` from standard input. `bolt` is told the socket in `SSH_AUTH_SOCK` and
  nothing else. A `passphrase` is answered to `ssh-add` from a file only you can read. The
  agent is stopped and its directory removed when the stage ends. Nothing of the key is on
  the command line, in the environment, in the compiled files or in a file Meridian keeps.
- **`agent: true`** names a key in your own agent: `bolt` reads `SSH_AUTH_SOCK` as it is,
  and Meridian starts nothing. **`from: ambient`** leaves signing in to the tool's own
  configuration, as with no credential at all.
- **Refused at compile**, rule `unsupported-credential`: a `certificate` (put it
  beside the key in an agent of your own and set `agent: true`); a `domain` (for WinRM
  hosts, which this plugin does not reach); a user name with a space or a line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). `puppet apply` configures this machine as the user running Meridian, whoever the document names. Leave the credential out, or name another host.

The plan line names the identity, `web (puppet) on web01.example.com as deploy (ssh,
user deploy)`, and never a value. `ssh-agent` and `ssh-add` are run from `PATH`, or from
`MERIDIAN_SSH_AGENT` and `MERIDIAN_SSH_ADD`.

The handover is tested against stand-ins for `bolt`, `ssh-agent` and `ssh-add` that keep what they are given (the inventory and its mode, the environment, the key and the passphrase). The real `bolt` is not driven in CI.

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type outside the six, or in the wrong case (`Package`) | use one of the six; there is no passthrough |
| `unsupported-param` | a param this type does not take on Puppet, including `package.version` and `exec.removes` | drop it, or put it in a Puppet module |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `invalid-param` | a param of the wrong kind: a list for `name`, a number for `mode` | quote the mode; give a single name |
| `unsupported-state` | a state this type does not have, or any state on an `exec` | see the table under [Resource types](#resource-types) |
| `param-conflicts-with-state` | `content` on a `directory` | drop one |
| `unsupported-runtime-condition` | `runtimeWhen` on any resource | decide with a compile-time `when` instead |
| `unsupported-action` | a notify action the type does not take: `reload`, `start`, `stop` on a service, anything on a package | use `restart`, or declare the state you want |
| `unknown-notify-target`, `unknown-dependency` | an edge names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource`, `duplicate-resource` | two resources with the same id | rename one |
| `duplicate-resource` | two resources that manage the same package, file, service, user or group | merge them |
| `invalid-name`, `invalid-name`, `invalid-name` | an id that cannot be a Puppet title | use letters, digits and underscores |
| `missing-param` | a resource with nothing to declare | give it params |
| `invalid-reference` | `${id.param}` names a resource or a param that is not there | fix the reference |
| `invalid-host` | a host that is not a name, so it cannot be written into a node statement | the core refuses one first ([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)) |
| `missing-host` | a ResourceSet with no host to write the node statement for | give it a `targetHost` |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run puppet: it is not installed, or not on the PATH` | the plugin runs `puppet` on the machine running Meridian | install the gem or the agent, or set `MERIDIAN_PUPPET` |
| the plan is unknown | Bolt's JSON report could not be read | run the `bolt apply` command the stage printed by hand and read its output |
| `puppet apply reported a failed resource` with status `4` or `6` | a resource failed on the node; the ones before it were applied | read the output above the message; `apply --resume` continues after the fix |
| `Could not find class meridian_web` | `site.pp` was run without `manifests/init.pp` beside it | run through Meridian, which joins them into `apply.pp` |
| a warning about `/etc/puppetlabs` or the home directory | Puppet writes settings and a cache under its confdir and vardir | point them at a scratch directory with `MERIDIAN_PUPPET_ARGS="--confdir ... --vardir ... --codedir ..."` |
| Bolt asks for a host key or a password | Bolt's own inventory and transport settings apply | put them in `MERIDIAN_PUPPET_ARGS` or Bolt's inventory file |

## How it is tested

- **Golden files** under `plugins/puppet/testdata/` record what every fixture
  and the example compile to, byte for byte, and what each refusal says.
- **The conformance tests** compile the same documents for every shared-type
  target and generate the [conformance matrix](../reference/conformance-matrix.md),
  which fails when this target starts accepting or refusing something new.
- **Run tests** (`plugins/puppet/tests/run.rs`) drive the runner against
  stand-ins for `puppet` and `bolt`: the exact commands, the exit statuses,
  and a Bolt report that cannot be read.
- **Real-tool tests** (`plugins/puppet/tests/real.rs`, workflow `puppet.yml`)
  run the real `puppet` from the gem: every fixture is parsed with
  `puppet parser validate`, a plan creates nothing, an apply converges files
  under a scratch directory, a second plan is in sync, drift is put right,
  and a failed resource fails the run and keeps what came before it. They
  are marked `#[ignore]` and run with
  `MERIDIAN_TEST_REAL_PUPPET=1 cargo test -p meridian-target-puppet --test real -- --ignored`.
  Reaching another machine with Bolt has only met a stand-in. See
  [Real-tool tests](../contributing/real-tool-tests.md).
