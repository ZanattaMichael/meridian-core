# Target: `chef`

<img src="../branding/logos/meridian-chef.svg" width="72" alt="Meridian Chef plugin logo">

Compiles a ResourceSet into a Chef cookbook and converges it with `chef-client`
in local mode on the node itself, or with `chef-run` for another machine. Chef
still does the work: Meridian writes the recipe a person would have written
and runs Chef on it.

## What it does and does not support

| | |
|---|---|
| Category | Configuration management |
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)), and any Chef resource beside them: `template`, `cron`, `native/package` ([Chef resources beside the six](#chef-resources-beside-the-six)) |
| `dependsOn` | Yes, as recipe order. A recipe runs top to bottom, so the dependency graph is flattened into one sequence |
| `notifies` | Native: `notifies`, delayed to the end of the run. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec, any action word on a Chef resource of its own. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: one line of Ruby in an `only_if` guard |
| Secrets | On this machine, yes: as node attributes from a JSON file only the user can read, with `sensitive true` on the resource. Through `chef-run`, no: it takes attributes on its command line only, so the apply stops. See [Secrets](#secrets) |
| References | `${id.param}` is settled from the document when it is compiled; anything else in `${...}` is refused, and `$${` is a literal `${` ([rules](../reference/resource-types.md#references)) |
| Plan | On this machine, yes: `--why-run`. For another machine, no: `chef-run` has no dry run, so the plan is reported as unknown |
| Apply | Yes |
| Outputs | No. A recipe produces no values for a later stage to read |
| Compiles to | `cookbooks/meridian_<name>/metadata.rb` and `recipes/default.rb` |
| Runs with | `chef-client --local-mode` on the node itself, `chef-run` for another host |

What it does not do:

- **No Chef server.** Plan and apply run in local mode from the cookbook on
  disk. The cookbook is an ordinary one, so you can upload it to a server with
  `knife` yourself.
- **A Chef resource of its own is passed through unchecked.** Its properties
  are written as the document gives them, and a property the resource does
  not have is Chef's to refuse when the recipe is compiled, not Meridian's.
  See [Chef resources beside the six](#chef-resources-beside-the-six).
- **No preview through `chef-run`.** A plan for another machine is reported as
  unknown, not as empty; see [Running](#running).
- **`package.version` and `exec.removes` are refused**, though Chef could
  express them; see [What is refused](#what-is-refused).
- **It creates no infrastructure.** It configures a node that already exists.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resource types](#resource-types) ·
[Chef resources beside the six](#chef-resources-beside-the-six) ·
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
| The machine Meridian runs on | `chef-client` for a local run. Cinc's `cinc-client` takes the same arguments; name it with [`MERIDIAN_CHEF_CLIENT`](#settings). `chef-run` (Chef Workstation) for another host |
| A host reached with `chef-run` | Whatever `chef-run` needs: SSH or WinRM and its own settings, passed through [`MERIDIAN_CHEF_ARGS`](#settings) |
| To compile only | Nothing. `meridian compile` never runs Chef |

Tested with Cinc Client 18, the community build of Chef Infra Client.

## A first document

[`examples/chef/web-server.yaml`](../../examples/chef/web-server.yaml)
installs nginx, writes its configuration, keeps it running and opens the
firewall on Debian-family hosts only. The part that shows what is particular
to Chef:

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

The same document compiles for Ansible, Puppet, Salt, OpenVox or cloud-init by
changing `target`, apart from `runtimeWhen`, which Puppet refuses; the
[conformance matrix](../reference/conformance-matrix.md) says where the
targets differ.

## Resource types

The six shared types, as this target takes them. A param not listed is refused
(`unsupported-param`); a state not listed is refused (`unsupported-state`).

| Type | Chef resource | States (`action`) | Params | Required |
|---|---|---|---|---|
| `package` | `package` | `present` (`:install`, default), `absent` (`:remove`), `latest` (`:upgrade`) | `name` (`package_name`) | `name` |
| `service` | `service` | `running` (`:start`, default), `started` (the same), `stopped` (`:stop`) | `name` (`service_name`), `enabled` | `name` |
| `file` | `file`, or `directory` for `state: directory` | `present` (`:create`, default), `file` (the same), `directory` (`:create`), `absent` (`:delete`) | `path`, `content`, `owner`, `group`, `mode` | `path` |
| `user` | `user` | `present` (`:create`, default), `absent` (`:remove`) | `name` (`username`), `uid`, `home`, `shell`, `system`, `groups` | `name` |
| `group` | `group` | `present` (`:create`, default), `absent` (`:remove`) | `name` (`group_name`), `gid`, `system` | `name` |
| `exec` | `execute` | none; an exec has no state and always has `action :run` | `command`, `chdir` (`cwd`), `creates`, `shell` | `command` |

Notes on how they map:

- A resource's **id is the Chef resource name**; the thing managed comes from
  `name` or `path`. Two resources that manage the same package, service, file,
  user or group are refused at compile time (`duplicate-resource`), where
  Chef would have converged them one after the other.
- A service's `enabled` is an action in Chef, not a property, so it is written
  as `:enable` or `:disable` in front of `:start` or `:stop`.
- A user's `groups` become one `group` block each, with `append true`, because
  Chef's `user` has a primary group only. A `group` resource is written with
  `append true` as well: a document declares the group, not its whole
  membership, and without it Chef would remove every member on each converge.
- `exec.shell` selects nothing. Chef hands a command that holds shell syntax
  to the shell and runs a plain one directly, so `shell: true` is accepted
  and changes nothing.
- `exec` runs on every converge unless `creates` says when it is already
  done, or a `runtimeWhen` guard says when to run it.
- A `mode` is written as Chef reads it: quote it in the document (`"0644"`),
  or the YAML reads it as a number and the compile refuses it.

## Chef resources beside the six

Any Chef resource is a resource type, written by the name a recipe uses:
`template`, `cron`, `remote_file`, `apt_repository`, `systemd_unit`, or a
resource a cookbook of your own defines. The resource's properties are the
resource's `params`, written into the block exactly as given; Meridian
checks nothing against a list of what the resource takes. A property the
resource does not have, or a resource no cookbook defines, is Chef's to
refuse when the recipe is compiled, as a provider's type is Terraform's,
until the resource's schema is synced and the editor reports it in place.

Five Chef resources have the names of the shared types: `package`,
`service`, `file`, `user` and `group`. The bare word is always the shared
type, so a document moves between tools unchanged. Chef's own `package`,
with `version`, `options` and every other property the shared type does not
take, is written `native/package`; the same for the other four. Chef runs a
command with `execute`, which has no shared name to collide with, so
`native/exec` is refused and `execute` is written as it is. The prefix is
only for those five names: `native/cron` is refused, because `cron` has one
spelling.

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
is a whole document of both kinds. A document that names a Chef resource
compiles for Chef only. The [`native-type`](../reference/lint.md#native-type)
lint rule marks each such resource. The six shared types stay the portable
way to say what they can say; a Chef resource is for everything else. The
split, and why the prefix is one word on every target, is
[decision 0032](../decisions/0032-native-types-beside-the-six-and-a-prefix-for-colliding-names.md).

## Ordering and notifications

`dependsOn` becomes the order of the recipe. Chef converges a recipe top to
bottom, so the resources are written in the apply order Meridian decided, and
resources that are independent of each other still run one after the other.

`notifies` becomes Chef's `notifies`, delayed to the end of the run, so a
service notified by three files is reloaded once:

| Notified type | Actions | What Chef does |
|---|---|---|
| `service` | `restart`, `reload`, `start`, `stop` | the action named, at the end of the run, if the notifier changed |
| `exec` | `run` | runs the command again |
| a Chef resource of its own | any action word | the action named, as the resource defines it |
| anything else | none | refused: `unsupported-action` |

`runtimeWhen` is the Ruby of an `only_if` guard, evaluated on the node, so it
can read node attributes. It must be one line; a longer one is refused
(`invalid-runtime-condition`).

## What a compile writes

```
cookbooks/meridian_<document name>/metadata.rb
cookbooks/meridian_<document name>/recipes/default.rb
```

`metadata.rb` names and versions the cookbook. `recipes/default.rb` holds one
block per resource, named by its id, in apply order. The cookbook is a
complete one, so it can be uploaded to a Chef server or vendored into a
policy as it is.

Because the cookbook's name comes from the document name, the name may hold
only lower-case letters, digits, hyphens and underscores. A name that cannot
be a cookbook name is refused, not rewritten (`invalid-name`).

Strings are written as Ruby strings: single-quoted where the text has no
escape in it, double-quoted with `\n`, `\"` and `\#` where it has. A value
Meridian placed from `--data`, `--set` or a data layer is written so Ruby
reads it as text and never as an interpolation
([values from data are literal](../reference/dsl/interpolation.md#a-tools-own-expressions)).

## Running

Chef has no way to push a cookbook to a node from a plain client, so there
are two ways to converge one, chosen by the document's host:

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine (`localhost`, `127.0.0.1`, `::1`) | `chef-client` | `chef-client --local-mode --no-color --force-formatter --override-runlist 'recipe[meridian_web]' --why-run` | the same without `--why-run` |
| anything else | `chef-run` | not available | `chef-run <host> cookbooks/meridian_web` |

Local mode takes the stage's work directory for its repository, because the
directory holds `cookbooks/`. Changes are read from chef-client's own report,
resource by resource: a line such as `* file[nginx_conf] action create`
followed by what it changed, or closed with `(up to date)` or
`(skipped due to only_if)`. A why-run plan lists every resource that would
change and touches none of them; the assumptions why-run prints under a
resource are not counted as changes.

`chef-run` cannot preview, so a plan for another machine is reported as
**unknown**, not as empty, and a run never says "nothing to do" on no
evidence. `chef-run` also does not say which resources it changed, so an apply
through it reports success without a list.

A failed resource fails the stage, with the end of Chef's output and the
changes made before it.

## Settings

| Variable | Meaning |
|---|---|
| `MERIDIAN_CHEF_CLIENT` | the client to run, when `chef-client` is not on `PATH`. Cinc's `cinc-client` takes the same arguments |
| `MERIDIAN_CHEF_RUN` | the program to run for another machine, when it is not `chef-run` on `PATH` |
| `MERIDIAN_CHEF_TRANSPORT` | `local` or `remote`, to override the choice made from the host |
| `MERIDIAN_CHEF_ARGS` | arguments added to every run, split like a shell command line (quotes hold a word together): `--chef-license accept`, or `chef-run`'s `--user deploy --identity-file ~/.ssh/deploy` |
| `MERIDIAN_SSH_AGENT` | the `ssh-agent` program, when it is not on `PATH`; started for a credential's key, see [Credentials](#credentials) |
| `MERIDIAN_SSH_ADD` | the `ssh-add` program, when it is not on `PATH` |

Every setting can also be written in the project's `meridian.yaml` under
`spec.settings.chef`, by its short name; the environment variable still wins.
The names, defaults and the splitting rule for argument lists are in the
[plugin settings reference](../reference/plugin-settings.md#chef).

## Secrets

A [secret](../reference/dsl/secrets.md) in a resource's params reaches Chef
as a node attribute, and through nothing else.

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

**What the compile writes.** The property reads the attribute under the
secret's key, and the resource is marked `sensitive`, so Chef prints neither
its diff nor its content, in a why-run or a converge:

```ruby
file 'db_conf' do
  content node['meridian_secret_db_password']
  mode '0600'
  path '/etc/app/db.conf'
  sensitive true
  action :create
end
```

The compiled recipe holds the key, never the value, so it can be reviewed and
kept. A resource without a secret is not made sensitive: its diff still shows.

**How the value reaches the tool.** On this machine, the runner writes an
attributes file with each secret under its key into a directory only the user
can enter (mode `0700`, the file `0600`, on a memory filesystem where the
system has one) and passes it as `--json-attributes <dir>/attributes.json`.
Local mode writes the node object, attributes and all, back to a `nodes`
directory under the repository after every run, which would leave the value
in the stage's work directory; so the run is also given a `client.rb` beside
the attributes, `--config <dir>/client.rb`, that names the work directory as
the repository and the private directory as `node_path`. Both files, and the
node Chef wrote, are overwritten and removed when the run ends. Neither the
command line nor the tool's environment ever holds the value, and nothing in
the work directory does.

**For another machine the apply stops.** `chef-run` takes attributes on its
command line only (`--json-attributes` is not among its options, and `--attr`
puts the value in `ps` for every user of this machine), so a document with a
secret and a host that is not this machine fails before anything is started:

```
chef-run has no private way to hand a secret to web01.example.com: it takes attributes on its command line only, where every user of this machine could read them. Run chef-client on the host, with the local transport, or keep the secret out of this document
```

A plan for that host is reported as unknown, as every `chef-run` plan is, and
runs nothing. To converge such a document, run Meridian on the node with
`MERIDIAN_CHEF_TRANSPORT=local`.

The handover is tested against a stand-in `chef-client` that keeps a copy of
the attributes file, its mode, the configuration, its arguments and its
environment, and against the real client in CI (`MERIDIAN_TEST_REAL_CHEF=1`),
which converges a file whose content is a secret and whose value then appears
in the file and in nothing Chef printed or the plugin wrote, the node file
included.

## Credentials

`targetHost.credential` names the [credential](../reference/dsl/credentials.md) the host is
reached as. Another machine is reached with `chef-run`, so the plugin takes an `ssh` credential with a user and a private key, and gives `chef-run` the two the way it takes them:

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
- A plan for another machine is unknown and runs nothing, as without a credential, so no agent is started for it.
- **A key is never on disk.** It goes into an SSH agent Meridian starts for the stage:
  `ssh-agent -D -a <socket>`, its socket in a directory only you can enter, loaded with
  `ssh-add -` from standard input. `chef-run` is told the socket in `SSH_AUTH_SOCK` and
  nothing else. A `passphrase` is answered to `ssh-add` from a file only you can read. The
  agent is stopped and its directory removed when the stage ends. Nothing of the key is on
  the command line, in the environment, in the compiled files or in a file Meridian keeps.
- **`agent: true`** names a key in your own agent: `chef-run` reads `SSH_AUTH_SOCK` as it is,
  and Meridian starts nothing. **`from: ambient`** leaves signing in to the tool's own
  configuration, as with no credential at all.
- **Refused at compile**, rule `unsupported-credential`: a `password` (`chef-run` takes one on its command line only, where every user of this machine could read it; reach the host as an `ssh` credential with a `privateKey`); a `certificate` (put it
  beside the key in an agent of your own and set `agent: true`); a `domain` (for WinRM
  hosts, which this plugin does not reach); a user name with a space or a line break.
- **Refused at run time:** a credential for this machine (`localhost`, `127.0.0.1`, `::1`
  or no host). `chef-client` configures this machine as the user running Meridian, whoever the document names. Leave the credential out, or name another host.

The plan line names the identity, `web (chef) on web01.example.com as deploy (ssh,
user deploy)`, and never a value. `ssh-agent` and `ssh-add` are run from `PATH`, or from
`MERIDIAN_SSH_AGENT` and `MERIDIAN_SSH_ADD`.

The handover is tested against stand-ins for `chef-run`, `ssh-agent` and `ssh-add` that keep what they are given: `chef-run` sees `--user deploy` and the socket, `ssh-add` the key and the passphrase, and the key is in nothing printed or left behind. The real `chef-run` is not driven in CI.

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

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

## How it is tested

- **Golden files** under `plugins/chef/testdata/` record what every fixture
  and the example compile to, byte for byte, and what each refusal says.
- **The conformance tests** compile the same documents for every shared-type
  target and generate the [conformance matrix](../reference/conformance-matrix.md),
  which fails when this target starts accepting or refusing something new.
- **Run tests** (`plugins/chef/tests/run.rs`) drive the runner against
  stand-ins for `chef-client` and `chef-run`: the exact commands, the report
  read back, and a why-run assumption that must not hide a change.
- **Real-tool tests** (`plugins/chef/tests/real.rs`, workflow `chef.yml`)
  run Cinc Client 18 in local mode: a why-run plan that creates nothing, a
  converge checked on disk, a second plan that is in sync, a notification, a
  `runtimeWhen` guard, a failing command that keeps what came before it, and,
  as root, a package, a group, a user and a service. They are marked
  `#[ignore]` and run with
  `MERIDIAN_TEST_REAL_CHEF=1 cargo test -p meridian-target-chef --test real -- --ignored`.
  A Chef server, and `chef-run` reaching another machine, have only met
  stand-ins. See [Real-tool tests](../contributing/real-tool-tests.md).
