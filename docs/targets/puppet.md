# Target: `puppet`

<img src="../branding/logos/meridian-puppet.svg" width="72" alt="Meridian Puppet plugin logo">

Compiles a ResourceSet into a Puppet class and node statement, applied with
`puppet apply` locally or Bolt for another machine.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `packageRepository`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any Puppet type beside them: `cron`, `mysql::db`, `native/file` ([Puppet types beside the six](#puppet-types-beside-the-six)) |
| `dependsOn` | Yes, as `require`. Puppet orders by declared relationships, so the dependency graph survives into the manifest intact |
| `notifies` | Native: `notify`. A service can be restarted (`restart` or `refresh`); an exec can be `run`; a Puppet type of its own can be refreshed (`refresh`) |
| `runtimeWhen` | No. A catalog is compiled before it reaches the node, so there is nothing left to evaluate at apply time. Refused |
| `adopt` | Accepted, and changes nothing: Puppet keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
| Secrets | Yes: by Hiera. The manifest looks each up as a `Sensitive` value, and the run is given a data file and a `hiera.yaml` only the user can read. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | Yes: `--noop`, with `--detailed-exitcodes`. Every resource reports whether it would change |
| Apply | Yes |
| Outputs | No. A manifest produces no values for a later stage to read |
| Editor schemas | Yes: `meridian schema sync` asks `puppet describe` what each type takes. See [Editor schemas](#editor-schemas) |
| Compiles to | `manifests/init.pp` and `site.pp` |
| Runs with | `puppet apply` on the node itself, `bolt apply` for another host |

Not supported:

- **`reload`, `start` and `stop` notifies** ([why](#ordering-and-notifications)).
- **A master or PuppetDB.** Runs are masterless; Hiera is used only for
  [secrets](#secrets). Copy the class to a master yourself.
- **Checking Puppet types' attributes**; Puppet checks them
  ([Puppet types](#puppet-types-beside-the-six)).
- **`package.version` and `exec.removes`** ([refused](#what-is-refused)).
- **Creating infrastructure.**

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | `puppet` for a local run (the `puppet` gem, or the puppet-agent package); `bolt` for another host |
| A host reached with Bolt | Whatever Bolt needs: SSH or WinRM, and Bolt's own inventory or `--targets` settings through [`MERIDIAN_PUPPET_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Puppet |

Tested with Puppet 8 (the gem) and OpenVox ([`openvox`](openvox.md) shares this compiler).

## A first document

[`examples/puppet/web-server.yaml`](../../examples/puppet/web-server.yaml):

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

It also compiles for the other CM targets; see the
[conformance matrix](../reference/conformance-matrix.md).

## Resource types

Unlisted params (`unsupported-param`) and states (`unsupported-state`) are refused.

| Type | Puppet type | States (`ensure`) | Params | Required |
|---|---|---|---|---|
| `package` | `package` | `present` (default), `absent`, `latest` | `name` | `name` |
| `packageRepository` | `yumrepo`; `format: apt` refused (use `apt::source`) | `present` (default), `absent` | `name`, `format`, `url` (`baseurl`), `key` (`gpgkey`) | `name`, `format`, `url` |
| `service` | `service` | `running` (default), `started` (the same), `stopped` | `name`, `enabled` (`enable`) | `name` |
| `file` | `file` | `file` (default), `present` (the same), `directory`, `link`, `absent` | `path`, `content`, `owner`, `group`, `mode`, `target` | `path`; `target` with `link` |
| `user` | `user` | `present` (default), `absent` | `name`, `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `group` | `present` (default), `absent` | `name`, `gid`, `system` | `name` |
| `exec` | `exec` | none; an exec has no state | `command`, `chdir` (`cwd`), `creates`, `shell` | `command` |

- The **id is the Puppet title**; the managed name comes from `name` or `path`. Two
  resources managing the same thing are refused (`duplicate-resource`).
- `file` with `state: directory` and `content` is refused (`param-conflicts-with-state`).
- `exec` with `shell: true` uses the shell provider; without it, pipes are arguments.
- `exec` runs every time unless `creates` is set. `unless`, `onlyif` and `runtimeWhen` are
  not available.
- Quote `mode` (`"0644"`).

## Puppet types beside the six

Any Puppet type is a resource type: `cron`, `mount`, `mysql::db`, `apache::vhost`. Its
`params` are its attributes, written as given; Puppet checks them when it compiles the
catalog.

The six shared names always mean the shared types. Puppet's own are `native/file`,
`native/package` and so on. `native/cron` is refused.

```yaml
resources:
  - id: html
    type: native/file
    state: directory
    params:
      path: /var/www/html
      source: puppet:///modules/web/html
      recurse: true
  - id: logrotate
    type: cron
    params:
      command: /usr/sbin/logrotate /etc/logrotate.conf
      user: root
      hour: 2
      minute: 0
    notifies:
      - resource: app_db
        action: refresh
  - id: app_db
    type: mysql::db
    params:
      user: app
      password: placeholder
```

compiles to

```puppet
  file { 'html':
    ensure  => directory,
    path    => '/var/www/html',
    recurse => true,
    source  => 'puppet:///modules/web/html',
  }

  cron { 'logrotate':
    command => '/usr/sbin/logrotate /etc/logrotate.conf',
    hour    => 2,
    minute  => 0,
    user    => 'root',
    notify  => Mysql::Db['app_db'],
  }

  mysql::db { 'app_db':
    password => 'placeholder',
    user     => 'app',
  }
```

| | |
|---|---|
| `type` | a Puppet type name: lower-case words of letters, digits and underscores, joined by `::`. `native/<name>` for the six colliding names. A wrong case (`Cron`), a dotted name, or `native/` on any other name is refused |
| `params` | the type's attributes, as written. A type that takes none is declared by its title alone |
| `state` | written as the type's `ensure`, whatever values the type gives it: a word is written bare (`present`, `running`), anything else quoted (a version). Writing both `state:` and `params.ensure` is refused (`param-conflicts-with-state`) |
| `notifies` to a Puppet type | action `refresh`: Puppet refreshes the declaration when a notifier changed, and whether the type does anything on refresh is the type's own business. Any other action is refused (`unsupported-action`) |
| `notifies` from a Puppet type | as from any resource |
| `runtimeWhen` | refused, as for the six |
| Secrets | as for the six: a `Sensitive` lookup the runner answers through Hiera |
| References | `${id.param}` is settled from the document, as for the six |
| `native/file` and a shared `file` on one path | refused (`duplicate-resource`), as two shared files would be: they are one Puppet type |

[`examples/puppet/native-types.yaml`](../../examples/puppet/native-types.yaml)
uses both. Such a document compiles for Puppet and [OpenVox](openvox.md) only; the
[`native-type`](../reference/lint.md#native-type) lint marks it. See
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering and notifications

`dependsOn` becomes `require`; resources are written in apply order. `notifies` becomes
`notify`:

| Notified type | Actions | What Puppet does |
|---|---|---|
| `service` | `restart`, `refresh` | restarts the service |
| `exec` | `run` | runs the command again, even if `creates` says it already ran |
| a Puppet type of its own | `refresh` | refreshes the declaration; what that does is the type's |
| anything else | none | refused: `unsupported-action` |

`reload`, `start` and `stop` are refused: Puppet's refresh only restarts. For a reload,
set the service's `restart` attribute in your own module.

## What a compile writes

```
manifests/init.pp
site.pp
```

`manifests/init.pp` holds class `meridian_<document name>`, one declaration per resource
titled by its id. `site.pp` holds the node statement. The document name must be lower-case
letters, digits and underscores.

Strings are single-quoted, or double-quoted with `\n`, `\"` and `\$` escapes. Values from
data are always literal
([see](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Running

The host decides how:

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine (`localhost`, `127.0.0.1`, `::1`) | `puppet` | `puppet apply --detailed-exitcodes --color=false --noop apply.pp` | the same without `--noop` |
| anything else | `bolt` | `bolt apply apply.pp --targets <host> --format json --noop` | the same without `--noop` |

`apply.pp` joins the class and node statement. Changes are read from Puppet's report.
Exit `2` is success with changes; `4` and `6` fail the stage, with what had changed. An
unreadable Bolt report makes the plan **unknown**.

### Reading a failure

Each resource that failed is a [resource failure](../reference/state.md#resource-failures), read from what Puppet writes to
standard error:

| Line | Becomes |
|---|---|
| `Error: /Stage[main]/.../Exec[migrate]/returns: change from ... failed: '/bin/false' returned 3 ...` | `migrate`, `resource-failed`, summary after `failed: `, tool code `rc=3` |
| `Warning: /Stage[main]/.../Service[app_svc]: Skipping because of failed dependencies` | `app_svc`, `dependency-failed` |
| `Error: Evaluation Error: ...` (or `Could not parse`, `Syntax error`, `Could not find`) | no resource, `invalid-config` |

Over Bolt, a `failure` event in the JSON report is the same on the target
named; a target Bolt could not reach is `unreachable`, with the error's
`kind` as the tool code. A message that names a refused login or a timeout
gives that cause instead.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_PUPPET` | the `puppet` program, when it is not on `PATH` |
| `MERIDIAN_BOLT` | the `bolt` program, when it is not on `PATH` |
| `MERIDIAN_PUPPET_TRANSPORT` | `local` or `bolt`, to override the choice made from the host |
| `MERIDIAN_PUPPET_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--confdir /tmp/p --vardir /tmp/v --codedir /tmp/c` for an isolated run, or Bolt's `--inventoryfile inventory.yaml` |
| `MERIDIAN_SSH_AGENT` | the `ssh-agent` program, when it is not on `PATH`; started for a credential's key, see [Credentials](#credentials) |
| `MERIDIAN_SSH_ADD` | the `ssh-add` program, when it is not on `PATH` |

Settings can also go in `meridian.yaml` under `spec.settings.puppet`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#puppet).

## Secrets

A [secret](../reference/dsl/secrets.md) reaches Puppet only through Hiera.

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

The param becomes a lookup in `Sensitive`, redacted in reports, `--noop` output and diffs.

```puppet
  file { 'db_conf':
    ensure  => file,
    content => Sensitive(lookup('meridian_secret_db_password')),
    mode    => '0600',
    path    => '/etc/app/db.conf',
  }
```

When a stage runs:

1. The runner writes `common.yaml` (mode `0600`, in a `0700` directory, in memory where
   possible) and a `hiera.yaml`.
2. The run gets `--hiera_config <dir>/hiera.yaml` (Bolt: `--hiera-config`). Bolt carries the
   value to the node in the catalog.
3. Both files are overwritten and removed after the run.

A secret inside a list or map is refused (`unsupported-secret-place`). Your own
`hiera.yaml` is not read during a run.

Tested against a stand-in `puppet` and real `puppet` in CI (`MERIDIAN_TEST_REAL_PUPPET=1`).

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md). `bolt`
reads it from an inventory written for the run:

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

- Leave `--inventoryfile` out of `MERIDIAN_PUPPET_ARGS` when a credential is named.
- **A key** goes into an SSH agent Meridian starts for the stage, loaded through `ssh-add`'s
  stdin; `bolt` gets only `SSH_AUTH_SOCK`. A `passphrase` is answered from a private file.
  See [0028](../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
- **`agent: true`** uses your own agent. **`from: ambient`** leaves signing in to the tool.
- **Refused at compile** (`unsupported-credential`): a `certificate` (load it in your own
  agent, `agent: true`), a `domain`, a user name with a space or line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). Leave the credential out, or name another host.

The plan names the identity (`web (puppet) on web01.example.com as deploy (ssh, user
deploy)`), never a value. `ssh-agent` and `ssh-add` come from `PATH` or `MERIDIAN_SSH_AGENT`
and `MERIDIAN_SSH_ADD`. Tested against stand-ins; real `bolt` is not run in CI.

## Editor schemas

`meridian schema sync` asks `puppet describe` for the Puppet types documents use
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | each Puppet type the documents use, under the name the document writes (`cron`, or `native/file` for one of the six names): every parameter and property with its description, the values it lists (`Valid values are ...`) as the param's values. The `Providers` and `Features` sections end the attributes: a provider's own attributes are not listed, and Puppet refuses one the type lacks at apply time. Read from `puppet describe <type>`, one run a type, with the target's own settings (`MERIDIAN_PUPPET`, `MERIDIAN_PUPPET_ARGS`). A defined type from a module (`mysql::db`) has no description `describe` can read, so it is a note and not the end of the sync |
| Listed by name | every other type `puppet describe --list` knows, with its short description; the six shared names under `native/`, since the bare words are the shared types |
| Pinned | Puppet's version, from `puppet --version` |

Params are text unless `describe` lists values. Every type takes `refresh`.

## What is refused

Each names the resource, line and rule:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type in the wrong case (`Package`, `Cron`), a dotted name, or `native/` on a name that is not one of the six | use one of the six, or a Puppet type as a declaration names it ([beside the six](#puppet-types-beside-the-six)) |
| `unsupported-param` | a param this type does not take on Puppet, including `package.version` and `exec.removes` | drop it, or put it in a Puppet module |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `invalid-param` | a param of the wrong kind: a list for `name`, a number for `mode` | quote the mode; give a single name |
| `unsupported-state` | a state this type does not have, or any state on an `exec` | see the table under [Resource types](#resource-types) |
| `param-conflicts-with-state` | `content` on a `directory`; `state` and `params.ensure` on a Puppet type of its own | drop one |
| `unsupported-runtime-condition` | `runtimeWhen` on any resource | decide with a compile-time `when` instead |
| `unsupported-action` | a notify action the type does not take: `reload`, `start`, `stop` on a service, anything on a package, anything but `refresh` on a Puppet type of its own | use `restart` or `refresh`, or declare the state you want |
| `unknown-notify-target`, `unknown-dependency` | an edge names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource` | two resources with the same id | rename one |
| `duplicate-resource` | two resources that manage the same package, file, service, user or group | merge them |
| `invalid-name` | an id that cannot be a Puppet title | use letters, digits and underscores |
| `missing-param` | a shared resource with nothing to declare | give it params |
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

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `--evaltrace`, `--debug`, or `--debug --trace` to `puppet apply`, and `--log-level info`, `debug` or `trace` to `bolt apply`. Bolt logs to standard error, so its JSON report is read as before.

## How it is tested

- **Golden files** in `plugins/puppet/testdata/`, refusals included.
- **Conformance tests** generate the [conformance matrix](../reference/conformance-matrix.md).
- **Run tests** (`plugins/puppet/tests/run.rs`) against stand-ins for `puppet` and `bolt`.
- **Real-tool tests** (`plugins/puppet/tests/real.rs`, `puppet.yml`): validate, plan, apply,
  drift and failure, run with
  `MERIDIAN_TEST_REAL_PUPPET=1 cargo test -p meridian-target-puppet --test real -- --ignored`.
  Bolt has only met a stand-in. See [Real-tool tests](../contributing/real-tool-tests.md).

## Inspecting one resource

[`meridian inspect`](../reference/cli.md#inspect) reads the resource by its type and namevar
(`path` for `file`, `command` for `exec`, otherwise `name`; the title when the declaration
sets none).

| Answer | From |
|---|---|
| actual | `puppet resource TYPE NAME --to_yaml`. A `content` checksum is left out. `ensure: absent` or `purged` is not found |
| as declared | a `--noop` run of the manifest: yes when it reports no event for the resource |

Over Bolt, the read is `bolt command run '/opt/puppetlabs/bin/puppet resource ...'` on the
node.
