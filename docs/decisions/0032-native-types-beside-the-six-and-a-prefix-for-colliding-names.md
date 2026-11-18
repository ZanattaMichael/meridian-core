# A configuration management target takes its tool's own types beside the six, and a native name a shared type has taken is written `native/<name>`

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | `meridian_sdk::TypeName`, read first by every configuration management compiler; the `native-type` lint rule |
| Where it shows | [The configuration management flavour](../reference/dsl/configuration-management.md#native-types-beside-the-six), each target's page under "beside the six", the [conformance matrix](../reference/conformance-matrix.md) |

## Context

The six configuration management targets took only `package`, `file`,
`service`, `user`, `group` and `exec`. Every other `type` was refused, so a
task with `lineinfile`, `template` or `git`, a Puppet `mysql::db`, a Salt
`pkgrepo.managed`, had no way to be written, and the Ansible importer left
such tasks out. The other twelve targets are native passthrough: `type` is
the tool's own type and `params` its own arguments, and coverage is the
tool's, not Meridian's (issue 163, from the resource coverage research in
issue 162).

Giving the configuration management targets the same passthrough raises one
question the other targets never had: the shared six are bare words, and
some tools' own types are bare words too. Puppet's core type for a file is
`file`; Chef's resource for a package is `package`. A document that writes
`type: file` on Puppet must mean one thing.

## The choice

Every configuration management target takes its tool's own types **beside**
the six, as the other targets do: `params` pass through to the tool as
written, nothing is checked against a hand-written list, and an argument the
tool does not take is the tool's to refuse when it runs, until the tool's
schema is synced and the editor reports it in place. `state` and `notifies`
map where the tool has the notion (Ansible's `state` argument, Puppet's
`ensure`, Chef's `action`) and are refused otherwise (Salt's function is its
state; a cloud-config module's keys say what it does). A notified native
resource does the one thing the tool can be asked for without knowing the
type: Ansible runs the task again (`run`), Puppet refreshes it (`refresh`),
Chef takes the action word as written, Salt makes the state watch the
notifier (`watch`), and cloud-init, which applies a module once, refuses.
Where the tool itself has no place for a native type, the target refuses
rather than guesses: nothing can be ordered around a cloud-config module,
because where cloud-init applies it is cloud-init's to know.

**The bare word is always the shared type.** `file` is the shared `file` on
every one of the six targets, whatever the tool calls its own. A native name
that is dotted or namespaced (`ansible.builtin.lineinfile`, `pkgrepo.managed`,
`mysql::db`) can never be a shared type, so it is written as the tool writes
it.

**A native type whose bare name is a shared type is written `native/<name>`.**
`native/file` on Puppet is Puppet's own `file`, with `source`, `recurse` and
every other attribute the shared type does not take; `native/package` on Chef
is Chef's `package`, with `options` and `version` as Chef takes them. The
prefix is one word, the same on every target, and `/` appears in no tool's own
type names, so it collides with nothing. It is read on every target and
refused where the tool has no bare type of that name: Ansible, whose modules
are always written in full, refuses `native/file` with
`ansible.builtin.file` as what to write; Salt, whose state functions are
always dotted, refuses it with `file.managed`; cloud-init, whose modules are
`packages` and `write_files` and not `package` and `file`, refuses it too,
and refuses those five keys as types, so that the shared type and a module
never write one key. The prefix is accepted only
for the six names a shared type has taken: `native/cron` on Puppet is refused
with `cron` as what to write, so there is one way to write each type.

The `native-type` lint rule (info) marks every resource whose type is the
tool's own: from that resource on, the document compiles for one target.

## Alternatives

**No native access to the few colliding types.** Simplest, but the colliding
types are the ones most often needed in their native form: Puppet's `file`
with `source` or `recurse`, `package` with `provider` or `install_options`,
`service` with `hasrestart`. Refusing those would leave the most common
reason for passthrough on Puppet and Chef unmet.

**The native name wins and the shared type is reached some other way.** A
document that compiled for six tools would change meaning on two of them,
and the shared dialect is the one place the language is "write once". The
shared six are the portable vocabulary; the tool's own types are the
exception that the document opts into.

**A target-named prefix, such as `puppet/file` or `chef:package`.** It says
which tool, but the document already does (`spec.target`), and a prefix per
target is six spellings of one idea. One word that means "the tool's own"
reads the same everywhere.

**Qualifying the tool's name the way the tool itself would.** Puppet has no
qualified spelling for its core `file`; Chef's is `Chef::Resource::File`,
which no cookbook writes. There is nothing native to borrow.
