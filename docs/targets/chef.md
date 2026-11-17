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
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)) |
| `dependsOn` | Yes, as recipe order. A recipe runs top to bottom, so the dependency graph is flattened into one sequence |
| `notifies` | Native: `notifies`, delayed to the end of the run. Actions `restart`, `reload`, `start`, `stop` on a service, `run` on an exec. The action is kept: a reload stays a reload |
| `runtimeWhen` | Yes: one line of Ruby in an `only_if` guard |
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
- **No resource types beyond the six.** There is no passthrough to Chef's
  other resources (`template`, `cron`, `remote_file` and the rest). Anything
  else is refused at compile time.
- **No preview through `chef-run`.** A plan for another machine is reported as
  unknown, not as empty; see [Running](#running).
- **`package.version` and `exec.removes` are refused**, though Chef could
  express them; see [What is refused](#what-is-refused).
- **It creates no infrastructure.** It configures a node that already exists.

Contents: [Requirements](#requirements) ·
[A first document](#a-first-document) ·
[Resource types](#resource-types) ·
[Ordering and notifications](#ordering-and-notifications) ·
[What a compile writes](#what-a-compile-writes) ·
[Running](#running) ·
[Settings](#settings) ·
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
be a cookbook name is refused, not rewritten (`invalid-cookbook-name`).

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
| `MERIDIAN_CHEF_ARGS` | arguments added to every run, split on spaces: `--chef-license accept`, or `chef-run`'s `--user deploy --identity-file ~/.ssh/deploy` |

## What is refused

Every refusal names the resource, the line and the rule. The rules this
target can raise, and what to do:

| Rule | Meaning | What to do |
|---|---|---|
| `unsupported-resource-type` | a type outside the six, or in the wrong case (`Package`) | use one of the six; there is no passthrough |
| `unsupported-param` | a param this type does not take on Chef, including `package.version` and `exec.removes` | drop it, or put it in a cookbook of your own |
| `missing-param` | no `name`, `path` or `command` | add it |
| `empty-param` | a required param that is empty text | give it a value |
| `invalid-param` | a param of the wrong kind: a list for `name`, a number for `mode` | quote the mode; give a single name |
| `unsupported-state` | a state this type does not have, or any state on an `exec` | see the table under [Resource types](#resource-types) |
| `invalid-runtime-condition` | a `runtimeWhen` that spans more than one line | write the guard on one line |
| `unsupported-action` | a notify action the type does not take: anything on a package, file, user or group | notify a service or an exec |
| `unknown-dependency` | an edge names a resource the document does not declare | fix the id |
| `ordering-violated` | the order the core handed over puts a resource before its dependency | a core bug; report it |
| `duplicate-resource` | two resources that manage the same package, service, file, user or group | merge them |
| `invalid-cookbook-name` | a document name that cannot be a cookbook name | use lower-case letters, digits, hyphens and underscores |
| `empty-cookbook` | a document with no resources | give it something to converge |
| `dangling-reference` | `${id.param}` names a resource or a param that is not there | fix the reference |

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
