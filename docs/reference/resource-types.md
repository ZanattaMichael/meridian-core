# The configuration management resource types

`ansible`, `puppet`, `openvox`, `chef`, `salt` and `cloud-init` share eight types. Puppet notes
hold for `openvox`. Anything outside the tables is refused. For more, use the
tool's own type ([native types](dsl/configuration-management.md#native-types-beside-the-six)).

Param kinds are checked once, the same on every target: text for names (`name`, `path`, `mode`,
`shell`), booleans for flags (`enabled`, `system`, `append`), integers for `uid` and `gid`,
lists for `groups`, `members` and `ssh_authorized_keys`. A wrong kind, including `yes`, is
`invalid-param` ([decision 0024](../decisions/0024-shared-param-kinds-are-checked-once-strictly.md)).
Secrets pass as text; empty params and secrets in lists or maps are refused (`empty-param`,
`unsupported-secret-place`).

See also [the configuration management flavour](dsl/configuration-management.md) and the
generated [conformance matrix](conformance-matrix.md). Other targets use the tool's own types
([see](../concepts/ir-overview.md#two-families-of-types)).

## `package`

| | |
|---|---|
| Params | `name` (required). Salt also takes `version`, with `present` only. cloud-init also takes `version`, to pin one |
| States | `present` (default), `absent`, `latest`. cloud-init: `present` only, because a first boot installs and cannot remove or track |
| Can be notified | no |

```yaml
- id: nginx_pkg
  type: package
  params:
    name: nginx
```

## `packageRepository`

| | |
|---|---|
| Params | `name`, `format` (`apt` or `yum`), `url` (all required). `key`: the signing key's URL. apt only: `distribution` (required), `components` (default `[main]`) |
| States | `present` (default), `absent`. cloud-init: `present` only |
| Can be notified | no |

| Target | apt | yum |
|---|---|---|
| ansible | `ansible.builtin.deb822_repository` | `ansible.builtin.yum_repository` |
| puppet, openvox | refused: no built-in apt source; use `apt::source` | `yumrepo` |
| chef | `apt_repository` | `yum_repository` |
| salt | `pkgrepo.managed` with the `deb` line, in `/etc/apt/sources.list.d/<name>.list` | `pkgrepo.managed` |
| cloud-init | `apt: sources: <name>.list`; `key` refused (cloud-init takes a key's text or id) | `yum_repos` |

`name` is the file name and repository id: letters, digits, `-`, `_`, `.`. A key turns on
signature checks for yum. cloud-init adds repositories before it installs packages, so a
`package` may depend on one.

```yaml
- id: nginx_repo
  type: packageRepository
  params:
    name: nginx
    format: apt
    url: https://nginx.org/packages/debian
    distribution: bookworm
    components: [nginx]
    key: https://nginx.org/keys/nginx_signing.key
```

## `service`

| | |
|---|---|
| Params | `name` (required), `enabled` (true or false: start at boot) |
| States | `running` (default), `started` (the same), `stopped`. Ansible and cloud-init also take `restarted` and `reloaded` |
| Can be notified | Ansible, Chef, Salt and cloud-init: `restart`, `reload`, `start`, `stop`. Puppet: `restart` (or `refresh`) only |

Puppet, Chef and Salt refuse `restarted` as a state; use `notifies`. Salt refuses `restart`
and `reload` on a `stopped` service. cloud-init compiles services to `systemctl` commands,
with a warning.

```yaml
- id: nginx_svc
  type: service
  state: running
  params:
    name: nginx
    enabled: true
```

## `file`

| | |
|---|---|
| Params | `path` (required), `content`, `owner`, `group`, `mode`, `target` (with `link`, required). cloud-init also takes `encoding` and `append` |
| States | `present` (default) or `file`, `directory`, `link`, `absent`. cloud-init: no `link` |
| Can be notified | no |

Quote `mode` (`"0644"`). A directory with `content` is refused. On Salt a file without
`content` is created if missing, directories get parents, and attributes on `absent` are
dropped with a warning. On cloud-init `present` needs `content`; `directory` and `absent`
become `install -d` and `rm -rf`.

`link` makes `path` a symbolic link to `target`. It takes `owner` and `group`, not
`content` or `mode` (`param-conflicts-with-state`). `target` without `link` is refused.

| Target | `link` |
|---|---|
| ansible | `ansible.builtin.file` with `state: link`, `src` |
| puppet, openvox | `file` with `ensure => link`, `target` |
| chef | `link`: `target_file`, `to` |
| salt | `file.symlink` |
| cloud-init | refused: use an `exec` running `ln -s` |

```yaml
- id: app_conf
  type: file
  params:
    path: /etc/app/app.conf
    content: "workers = 8\n"
    owner: root
    mode: "0640"
```

## `user`

| | |
|---|---|
| Params | `name` (required), `uid`, `home`, `shell`, `system`, `groups`. cloud-init also takes `gecos`, `primary_group`, `lock_passwd`, `sudo` and `ssh_authorized_keys` |
| States | `present` (default), `absent`. cloud-init: `present` only |
| Can be notified | no |

`groups` are supplementary. Chef adds a membership block per group. On Salt the list is
complete.

## `group`

| | |
|---|---|
| Params | `name` (required), `gid`, `system`. cloud-init takes `name` and `members` only: it cannot choose a group's id |
| States | `present` (default), `absent`. cloud-init: `present` only |
| Can be notified | no |

## `cron`

| | |
|---|---|
| Params | `name`, `command` (required); `minute`, `hour`, `day`, `month`, `weekday` (default `*`, one cron field each, number or text); `user` (default `root`) |
| States | `present` (default), `absent`. cloud-init: `present` only |
| Can be notified | no |

| Target | Compiles to |
|---|---|
| ansible | `ansible.builtin.cron` (`job`, `name`) |
| puppet, openvox | `cron` (`monthday` for `day`) |
| chef | `cron` named by `name` |
| salt | `cron.present` / `cron.absent`, `identifier` from `name` (`daymonth`, `dayweek`) |
| cloud-init | a file `/etc/cron.d/<name>`; `name` is letters, digits, `-`, `_` |

`name` identifies the job, so changing the schedule updates it in place. The tool's own
cron type is `native/cron`.

```yaml
- id: rotate
  type: cron
  params:
    name: logrotate
    command: /usr/sbin/logrotate /etc/logrotate.conf
    minute: 0
    hour: 2
```

## `exec`

Runs a command. No state.

| | |
|---|---|
| Params | `command` (required), `chdir`, `creates` (skip when this path exists), `shell` (true to run through a shell). Ansible, Salt and cloud-init also take `removes` (run only when this path exists) |
| States | none |
| Can be notified | `run` |

It runs every apply unless `creates` is set, so plans always show it. A notified `exec`
runs again when notified. On cloud-init it runs once and stops later commands on failure.
Salt refuses shell syntax without `shell: true` ([Salt](../targets/salt.md#exec)).

```yaml
- id: warm_cache
  type: exec
  params:
    command: /usr/local/bin/warm-cache --all
    chdir: /srv/app
    creates: /var/cache/app/warm
```

## References

`${id.param}` is settled at compile to that param's value on resource `id`.

```yaml
- id: site_root
  type: file
  state: directory
  params:
    path: /srv/app

- id: index
  type: file
  params:
    path: ${site_root.path}/index.html   # /srv/app/index.html
    content: hello
```

A whole-value reference keeps its type; inside text it must be a single value. It does not
order resources. Anything unresolvable is `invalid-reference`. `$${` is a literal `${`.
`runtimeWhen` is untouched.

## What every resource can carry

| Field | Meaning | Where |
|---|---|---|
| `dependsOn` | order | every target ([how](../concepts/dependencies.md)) |
| `notifies` | react to a change | where the tool can ([matrix](capability-matrix.md)) |
| `runtimeWhen` | a condition checked on the node during the apply, written in the tool's own language | Ansible (a `when:` expression), Chef (the Ruby of an `only_if` guard), Salt (a Jinja expression over grains and pillar). Puppet refuses it: a catalog is compiled before it reaches the node. cloud-init refuses it: it evaluates nothing per resource |
| `adopt` | the tool's identifier of a thing that already exists, to take under management instead of creating ([how](dsl/resources.md#adopting-what-exists)) | Terraform, OpenTofu and Pulumi import it; the configuration management targets, Kubernetes, Bicep and ARM accept it and change nothing; Docker takes `adopt: replace` and removes then remakes the container; the rest refuse it ([matrix](capability-matrix.md)) |
