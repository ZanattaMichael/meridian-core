# Target: `chef`

<img src="../branding/logos/meridian-chef.svg" width="72" alt="Meridian Chef plugin logo">

Compiles a ResourceSet into a Chef cookbook, converged with `chef-client` in local mode,
or `chef-run` for another machine.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `packageRepository`, `file`, `service`, `user`, `group`, `cron`, `exec` ([reference](../reference/resource-types.md)), and any Chef resource beside them: `template`, `remote_file`, `native/package` ([Chef resources beside the six](#chef-resources-beside-the-six)) |
| `dependsOn` | Yes, as recipe order. A recipe runs top to bottom, so the dependency graph is flattened into one sequence |
| `notifies` | Native: `notifies`, delayed to the end of the run. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec, any action word on a Chef resource of its own. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: one line of Ruby in an `only_if` guard |
| `adopt` | Accepted, and changes nothing: Chef keeps no state and makes each thing what the document says whether it exists or not. The compile warns |
| Secrets | On this machine, yes: as node attributes from a JSON file only the user can read, with `sensitive true` on the resource. Through `chef-run`, no: it takes attributes on its command line only, so the apply stops. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | On this machine, yes: `--why-run`. For another machine, no: `chef-run` has no dry run, so the plan is reported as unknown |
| Apply | Yes |
| Outputs | No. A recipe produces no values for a later stage to read |
| Editor schemas | Yes: `meridian schema sync` asks `chef-resource-inspector` what each resource takes. See [Editor schemas](#editor-schemas) |
| Compiles to | `cookbooks/meridian_<name>/metadata.rb` and `recipes/default.rb` |
| Runs with | `chef-client --local-mode` on the node itself, `chef-run` for another host |

Not supported:

- **A Chef server.** Runs use local mode; upload the cookbook with `knife` yourself.
- **Checking Chef resources' properties**; Chef checks them
  ([Chef resources](#chef-resources-beside-the-six)).
- **Previews through `chef-run`**: such plans are unknown ([Running](#running)).
- **`package.version` and `exec.removes`** ([refused](#what-is-refused)).
- **Creating infrastructure.**

## Requirements

| Where | What |
|---|---|
| The machine Meridian runs on | `chef-client` for a local run. Cinc's `cinc-client` takes the same arguments; name it with [`MERIDIAN_CHEF_CLIENT`](#settings). `chef-run` (Chef Workstation) for another host |
| A host reached with `chef-run` | Whatever `chef-run` needs: SSH or WinRM and its own settings, passed through [`MERIDIAN_CHEF_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Chef |

Tested with Cinc Client 18, the community build of Chef Infra Client.

## A first document

[`examples/chef/web-server.yaml`](../../examples/chef/web-server.yaml) installs nginx and
opens the firewall on Debian-family hosts only:

```yaml
spec:
  target: chef
  targetHost:
    from: inline
    host: web01.example.com
  resources:
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
      # Chef's notifies names the action, so a reload stays a reload.
      notifies:
        - resource: nginx_svc
          action: reload
    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
    - id: open_firewall
      type: exec
      params:
        command: ufw allow 80/tcp
      runtimeWhen: "node['platform_family'] == 'debian'"
```

It compiles to
([the whole recipe](../../plugins/chef/testdata/web-server.cookbooks_meridian_web_recipes_default_rb)):

```ruby
file 'nginx_conf' do
  content "server {\n  listen 80;\n  root /srv/www;\n}\n"
  mode '0644'
  owner 'root'
  path '/etc/nginx/conf.d/site.conf'
  notifies :reload, 'service[nginx_svc]', :delayed
  action :create
end

service 'nginx_svc' do
  service_name 'nginx'
  action [:enable, :start]
end

execute 'open_firewall' do
  command 'ufw allow 80/tcp'
  only_if { node['platform_family'] == 'debian' }
  action :run
end
```

It compiles for the other CM targets too, except `runtimeWhen` on Puppet; see the
[conformance matrix](../reference/conformance-matrix.md).

## Resource types

Unlisted params (`unsupported-param`) and states (`unsupported-state`) are refused.

| Type | Chef resource | States (`action`) | Params | Required |
|---|---|---|---|---|
| `package` | `package` | `present` (`:install`, default), `absent` (`:remove`), `latest` (`:upgrade`) | `name` (`package_name`) | `name` |
| `packageRepository` | `apt_repository` (apt), `yum_repository` (yum) | `present` (`:add` / `:create`, default), `absent` (`:remove`) | `name` (`repo_name` / `repositoryid`), `format`, `url`, `key`; apt: `distribution`, `components` | `name`, `format`, `url` |
| `service` | `service` | `running` (`:start`, default), `started` (the same), `stopped` (`:stop`) | `name` (`service_name`), `enabled` | `name` |
| `file` | `file`; `directory` for `state: directory`; `link` for `state: link` | `present` (`:create`, default), `file` (the same), `directory` (`:create`), `link` (`:create`), `absent` (`:delete`) | `path` (`target_file` on a link), `content`, `owner`, `group`, `mode`, `target` (`to`) | `path`; `target` with `link` |
| `user` | `user` | `present` (`:create`, default), `absent` (`:remove`) | `name` (`username`), `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `group` | `present` (`:create`, default), `absent` (`:remove`) | `name` (`group_name`), `gid`, `system` | `name` |
| `cron` | `cron`, named by `name` | `present` (`:create`, default), `absent` (`:delete`) | `name`, `command`, `user`, `minute`, `hour`, `day`, `month`, `weekday` | `name`, `command` |
| `exec` | `execute` | none; an exec has no state and always has `action :run` | `command`, `chdir` (`cwd`), `creates`, `shell` | `command` |

- The **id is the Chef resource name**; the managed thing comes from `name` or `path`. Two
  resources managing the same thing are refused (`duplicate-resource`).
- A service's `enabled` becomes `:enable` or `:disable` before `:start` or `:stop`.
- A user's `groups` become one `group` block each with `append true`; `group` resources use
  `append true` too, so members are not removed.
- `exec.shell` is accepted and changes nothing; Chef picks the shell itself.
- `exec` runs every converge unless `creates` or `runtimeWhen` says otherwise.
- Quote `mode` (`"0644"`).

## Chef resources beside the six

Any Chef resource is a type: `template`, `remote_file`, `apt_repository`, or your
cookbook's own. `params` are its properties, written as given; Chef checks them.

The shared names mean the shared types; Chef's own are `native/package`, `native/cron`
and so on. Use `execute` as is; `native/exec` and `native/template` are refused.

```yaml
resources:
  - id: nginx_pkg
    type: native/package
    state: upgrade
    params:
      package_name: nginx
      options: "--no-install-recommends"
  - id: nginx_conf
    type: template
    params:
      path: /etc/nginx/nginx.conf
      source: nginx.conf.erb
      variables: {workers: 4}
    notifies:
      - resource: nginx_svc
        action: reload
  - id: refresh_index
    type: apt_update
    runtimeWhen: "node['platform_family'] == 'debian'"
```

compiles to

```ruby
package 'nginx_pkg' do
  options '--no-install-recommends'
  package_name 'nginx'
  action :upgrade
end

template 'nginx_conf' do
  path '/etc/nginx/nginx.conf'
  source 'nginx.conf.erb'
  variables({ 'workers' => 4 })
  notifies :reload, 'service[nginx_svc]', :delayed
end

apt_update 'refresh_index' do
  only_if { node['platform_family'] == 'debian' }
end
```

| | |
|---|---|
| `type` | a Chef resource name: one lower-case word of letters, digits and underscores, as Ruby writes a method. `native/<name>` for the five colliding names. A wrong case (`Template`), a dotted or `::` name, `native/exec`, or `native/` on any other name is refused |
| `params` | the resource's properties, as written. A hash is written in parentheses, as Ruby needs it. A resource that takes none is written with none |
| `state` | written as the resource's `action`, whatever actions the resource has (`create`, `upgrade`, `enable`). No state means no `action` line: the resource takes its own default action. Writing both `state:` and `params.action` is refused (`param-conflicts-with-state`); a state that is not one lower-case word is refused (`unsupported-state`) |
| `notifies` to a Chef resource | any action word the resource has, written as given: `notifies :run, 'apt_update[x]', :delayed`. Which actions the resource has is Chef's to know, until its schema is synced. An action that is not one lower-case word is refused (`unsupported-action`) |
| `notifies` from a Chef resource | as from any resource |
| `runtimeWhen` | an `only_if` guard, as for the six |
| Secrets | as for the six: a node attribute the runner hands over, and `sensitive true` on the block |
| References | `${id.param}` is settled from the document, as for the six |
| `native/package` and a shared `package` of one name | refused (`duplicate-resource`), as two shared packages would be: they are one Chef resource |

[`examples/chef/native-resources.yaml`](../../examples/chef/native-resources.yaml)
uses both. Such a document compiles for Chef only; the
[`native-type`](../reference/lint.md#native-type) lint marks it. See
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering and notifications

`dependsOn` becomes recipe order. `notifies` becomes a delayed `notifies`, so a service
notified three times reloads once:

| Notified type | Actions | What Chef does |
|---|---|---|
| `service` | `restart`, `reload`, `start`, `stop` | the action named, at the end of the run, if the notifier changed |
| `exec` | `run` | runs the command again |
| a Chef resource of its own | any action word | the action named, as the resource defines it |
| anything else | none | refused: `unsupported-action` |

`runtimeWhen` is one line of Ruby for an `only_if` guard (`invalid-runtime-condition`
otherwise).

## What a compile writes

```
cookbooks/meridian_<document name>/metadata.rb
cookbooks/meridian_<document name>/recipes/default.rb
```

`recipes/default.rb` holds one block per resource in apply order. The document name must be
lower-case letters, digits, hyphens and underscores (`invalid-name`).

Strings are single-quoted, or double-quoted with `\n`, `\"` and `\#` escapes. Values from
data are always literal ([see](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Running

The host decides how:

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine (`localhost`, `127.0.0.1`, `::1`) | `chef-client` | `chef-client --local-mode --no-color --force-formatter --override-runlist 'recipe[meridian_web]' --why-run` | the same without `--why-run` |
| anything else | `chef-run` | not available | `chef-run <host> cookbooks/meridian_web` |

Changes are read from chef-client's report. A why-run plan touches nothing; why-run
assumptions are not changes. Through `chef-run` a plan is **unknown** and an apply reports
no list. A failed resource fails the stage, with what had changed.

### Reading a failure

`chef-client`'s formatter names the failed resource; it is a
[resource failure](../reference/state.md#resource-failures):

```
Error executing action `run` on resource 'execute[migrate]'
Mixlib::ShellOut::ShellCommandFailed
Expected process to exit with [0], but received '3'
```

| From the output | Becomes |
|---|---|
| `resource 'execute[migrate]'` | the resource, `migrate` |
| the exception's message | the summary |
| `received 'N'`, else the exception class | the tool code, `rc=3` |
| `Recipe Compile Error` | no resource, `invalid-config` |
| anything else | `resource-failed`, `timed-out` for a command timeout, or `unreachable`, `auth-failed`, `timed-out` or `reboot-required` when the message says so |

`chef-run` reports no failures; its own error stands.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_CHEF_CLIENT` | the client to run, when `chef-client` is not on `PATH`. Cinc's `cinc-client` takes the same arguments |
| `MERIDIAN_CHEF_RUN` | the program to run for another machine, when it is not `chef-run` on `PATH` |
| `MERIDIAN_CHEF_TRANSPORT` | `local` or `remote`, to override the choice made from the host |
| `MERIDIAN_CHEF_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--chef-license accept`, or `chef-run`'s `--user deploy --identity-file ~/.ssh/deploy` |
| `MERIDIAN_CHEF_INSPECTOR` | the resource inspector `meridian schema sync` runs, when it is not `chef-resource-inspector` on `PATH`; `cinc-resource-inspector`, then the copies the Cinc and Chef packages keep under `/opt/cinc` and `/opt/chef`, are tried when that does not run. See [Editor schemas](#editor-schemas) |
| `MERIDIAN_SSH_AGENT` | the `ssh-agent` program, when it is not on `PATH`; started for a credential's key, see [Credentials](#credentials) |
| `MERIDIAN_SSH_ADD` | the `ssh-add` program, when it is not on `PATH` |

Settings can also go in `meridian.yaml` under `spec.settings.chef`; the environment wins.
See the [plugin settings reference](../reference/plugin-settings.md#chef).

## Secrets

A [secret](../reference/dsl/secrets.md) reaches Chef only as a node attribute.

```yaml
spec:
  target: chef
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

The property reads the attribute, and the resource is marked `sensitive`:

```ruby
file 'db_conf' do
  content node['meridian_secret_db_password']
  mode '0600'
  path '/etc/app/db.conf'
  sensitive true
  action :create
end
```

On this machine:

1. The runner writes `attributes.json` (mode `0600`, in a `0700` directory, in memory where
   possible), passed by `--json-attributes`.
2. `--config <dir>/client.rb` sets `node_path` to that directory, so the node file stays out
   of the work directory.
3. All of it is removed after the run.

**For another machine the apply stops**, because `chef-run` takes attributes only on its
command line:

```
chef-run has no private way to hand a secret to web01.example.com: it takes attributes on its command line only, where every user of this machine could read them. Run chef-client on the host, with the local transport, or keep the secret out of this document
```

Run Meridian on the node with `MERIDIAN_CHEF_TRANSPORT=local` instead.

Tested against a stand-in `chef-client` and the real client in CI
(`MERIDIAN_TEST_REAL_CHEF=1`).

## Credentials

`targetHost.credential` names an `ssh` [credential](../reference/dsl/credentials.md) with a
user and a private key:

```yaml
spec:
  target: chef
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

- **The user** is `chef-run`'s `--user`, before the host.
- A plan starts no agent.
- **A key** goes into an SSH agent Meridian starts for the stage, loaded through `ssh-add`'s
  stdin; `chef-run` gets only `SSH_AUTH_SOCK`. A `passphrase` is answered from a private file.
  See [0028](../decisions/0028-a-key-reaches-a-host-through-an-agent-meridian-starts.md).
- **`agent: true`** uses your own agent. **`from: ambient`** leaves signing in to the tool.
- **Refused at compile** (`unsupported-credential`): a `password` (it would be on the
  command line; use a `privateKey`), a `certificate` (load it in your own agent,
  `agent: true`), a `domain`, a user name with a space or line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). Leave the credential out, or name another host.

The plan names the identity (`web (chef) on web01.example.com as deploy (ssh, user
deploy)`), never a value. `ssh-agent` and `ssh-add` come from `PATH` or `MERIDIAN_SSH_AGENT`
and `MERIDIAN_SSH_ADD`. Tested against stand-ins; real `chef-run` is not run in CI.

## Editor schemas

`meridian schema sync` asks `chef-resource-inspector` for the Chef resources documents use
([Tool schemas](../tooling/schema-sync.md)).

| | |
|---|---|
| Described in full | each Chef resource the documents use, under the name the document writes (`template`, or `native/package` for one of the six names): every property with its kind from the Ruby types the inspector lists (`String` as text, `Integer`, `true`/`false` as a flag, `Array`, `Hash`; anything else as any value), whether it is required, its `equal_to` values, the resource's description, and its actions beside `nothing`, which is what `notifies` can name. Read from the inspector's one JSON document, which comes with the Chef or Cinc client (`cinc-resource-inspector`, then the copies the Cinc and Chef packages keep under `/opt/cinc` and `/opt/chef`, are tried when `chef-resource-inspector` does not run). A custom resource from a cookbook is not in it, so it is a note and not the end of the sync |
| Listed by name | every other resource the inspector knows, with its description; the six shared names under `native/` |
| Pinned | the client's version, from `chef-client --version` |

A property required for some actions is marked required. No property is marked
sensitive.

## What is refused

Each names the resource, line and rule:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type in the wrong case (`Package`, `Template`), a dotted or `::` name, `native/exec`, or `native/` on a name that is not one of the six | use one of the six, or a Chef resource as a recipe names it ([beside the six](#chef-resources-beside-the-six)) |
| `unsupported-param` | a param this type does not take on Chef, including `package.version` and `exec.removes` | drop it, or put it in a cookbook of your own |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `invalid-param` | a param of the wrong kind: a list for `name`, a number for `mode` | quote the mode; give a single name |
| `unsupported-state` | a state this type does not have, any state on an `exec`, or a state on a Chef resource of its own that is not one lower-case action word | see the table under [Resource types](#resource-types) |
| `param-conflicts-with-state` | `state` and `params.action` both on a Chef resource of its own | drop one |
| `invalid-runtime-condition` | a `runtimeWhen` that spans more than one line | write the guard on one line |
| `unsupported-action` | a notify action the type does not take: anything on a package, file, user or group; an action on a Chef resource of its own that is not one lower-case word | notify a service, an exec, or a Chef resource of its own |
| `unknown-dependency` | an edge names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource` | two resources that manage the same package, service, file, user or group | merge them |
| `invalid-name` | a document name that cannot be a cookbook name | use lower-case letters, digits, hyphens and underscores |
| `empty-document` | a document with no resources | give it something to converge |
| `invalid-reference` | `${id.param}` names a resource or a param that is not there | fix the reference |

## Troubleshooting

| What you see | Why | What to do |
|---|---|---|
| `could not run chef-client: it is not installed, or not on the PATH` | the plugin runs `chef-client` on the machine running Meridian | install Chef Infra Client or Cinc, or set `MERIDIAN_CHEF_CLIENT` |
| Chef asks you to accept its license | Chef Infra Client needs the license accepted once | set `MERIDIAN_CHEF_ARGS=--chef-license accept`, or use Cinc |
| the plan is unknown | the host is another machine, and `chef-run` has no dry run | run the plan on the node itself, or read the apply's report |
| `chef-client could not converge the cookbook` | a resource failed on the node; the ones before it were converged | read the output above the message; `apply --resume` continues after the fix |
| a package, user or service is "up to date" when it is not | chef-client was run without root, so it could not look | run Meridian as root, or set `MERIDIAN_CHEF_CLIENT` to a wrapper that uses `sudo` |
| `chef-run cannot be given this host` | the host is not a plain host name | fix `targetHost`; the core refuses one first ([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)) |

[`--tool-debug`](../reference/cli.md#asking-a-tool-for-more-output) adds `-l info`, `-l debug` or `-l trace` to a local `chef-client`. `chef-run` has no log flag: it is given `--config` with your `~/.chef-workstation/config.toml` and a `[log]` level of `info` (level 1) or `debug` (2 and 3), and its log is copied into the step's output when it ends.

## How it is tested

- **Golden files** in `plugins/chef/testdata/`, refusals included.
- **Conformance tests** generate the [conformance matrix](../reference/conformance-matrix.md).
- **Run tests** (`plugins/chef/tests/run.rs`) against stand-ins for `chef-client` and
  `chef-run`.
- **Real-tool tests** (`plugins/chef/tests/real.rs`, `chef.yml`) run Cinc Client 18: plan,
  converge, notify, `runtimeWhen`, failure, and as root a package, group, user and service.
  Run with
  `MERIDIAN_TEST_REAL_CHEF=1 cargo test -p meridian-target-chef --test real -- --ignored`.
  A Chef server and `chef-run` have only met stand-ins. See
  [Real-tool tests](../contributing/real-tool-tests.md).

## Inspecting one resource

Chef cannot read one resource alone. [`meridian inspect`](../reference/cli.md#inspect) runs
`chef-client --local-mode --why-run` on this machine.

| Answer | From |
|---|---|
| actual | the lines why-run prints under the resource, as `wouldChange` |
| as declared | yes when why-run would not change the resource |

On another machine `chef-run` has no why-run mode, and inspect is refused with
`unsupported-inspect`.
