# Target: `puppet`

<img src="../branding/logos/meridian-puppet.svg" width="72" alt="Meridian Puppet plugin logo">

Compiles a ResourceSet into a Puppet class and the node statement that assigns
it, and applies it with `puppet apply`, or with Bolt for another machine.

## What it does and does not support

| | |
|---|---|
| Resource types | `package`, `file`, `service`, `user`, `group`, `exec` ([reference](../reference/resource-types.md)) |
| `dependsOn` | Yes, as `require`. Puppet orders by declared relationships, so the dependency graph survives into the manifest intact |
| `notifies` | Native: `notify`. A service can be restarted (`restart` or `refresh`); an exec can be `run` |
| Not supported | `action: reload`, `start` or `stop` on a service. Puppet's refresh restarts a service and takes no argument. The compile refuses these and says why; it does not turn a reload into a restart |
| `runtimeWhen` | No. A catalog is compiled before it reaches the node, so there is nothing left to evaluate at apply time. Refused |
| Plan | Yes: `--noop` |
| Outputs | No |

## What a document compiles to

`manifests/init.pp` holds a class named `meridian_<document name>` with one
resource declaration a resource, titled by its id. `site.pp` holds the node
statement that includes the class on the document's host. They are separate so a
site that classifies nodes some other way can use the class alone.

Because the class name comes from the document name, the name may hold only
lower-case letters, digits and underscores. A name that cannot be a class name
is refused, not rewritten.

Puppet identifies a managed thing by its name or path, not by the declaration's
title. Two resources that manage the same package or file would only collide on
the node, so Meridian refuses them at compile time.

The node statement in `site.pp` is written as a Puppet string, with the same quoting
as every other string the plugin writes, and the plugin refuses a host that is not a
name (rule `invalid-host`); the core refuses one before it gets this far
([`targetHost`](../reference/dsl/outputs-and-hand-offs.md#rules)).

## Example

[`examples/puppet/web-server.yaml`](../../examples/puppet/web-server.yaml) compiles to:

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

## Running

Puppet has no way to push a catalog to a node, so there are two ways to apply
one, chosen by the document's host:

| Host | Tool | Plan | Apply |
|---|---|---|---|
| this machine (`localhost`) | `puppet` | `puppet apply --detailed-exitcodes --noop apply.pp` | the same without `--noop` |
| anything else | `bolt` | `bolt apply apply.pp --targets <host> --noop` | the same without `--noop` |

| Variable | Meaning |
|---|---|
| `MERIDIAN_PUPPET`, `MERIDIAN_BOLT` | the programs to run, when they are not on `PATH` |
| `MERIDIAN_PUPPET_TRANSPORT` | `local` or `bolt`, to override the choice made from the host |
| `MERIDIAN_PUPPET_ARGS` | arguments added to every run |

When Bolt's report cannot be read, the plan is reported as unknown, not as
empty.
