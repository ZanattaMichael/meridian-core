# Compatibility

Which versions of what work together. Meridian has four things that carry a
version, and each moves on its own clock:

| What | Looks like | Where you meet it |
|---|---|---|
| The core | `0.1.0`, [SemVer](https://semver.org) | `meridian --version` |
| The language | `meridian/v1`, a whole number | `apiVersion` at the top of every document |
| The plugin SDK | `0.1.0`, SemVer | the release a plugin was built against |
| The plugin protocol | `2`, a whole number | agreed between `meridian` and each plugin when it starts |

The tables on this page are generated:
`crates/meridian-cli/tests/compatibility.rs` writes them from the constants in
the code and fails when they are out of date, so they cannot drift from what the
build does. [`meridian version`](cli.md#version) prints the same facts for the
build you have, and [`meridian plugins`](cli.md#plugins) shows the SDK and the
protocol each plugin ended up speaking.

The policy behind all of this, including how long a version is supported, is
[release and versioning](../plans/release-and-versioning.md); the design is
[modules and versioning](../plans/modules-and-versioning.md).

## This build

| | |
|---|---|
| Core | `0.1.0` |
| Language versions read | `meridian/v1` |
| Plugin SDK shipped | `0.1.0` |
| Plugin protocol versions spoken | 2 |
| [Lockfile](modules.md#the-lockfile) format written | 1 |

## Language versions

Every document says which version of the language it is written in. The
version is looked up before anything else in the document is read.

| `apiVersion` | In this build | What happens to a document written in it |
|---|---|---|
| `meridian/v1` | supported | It compiles |

Anything else is refused, and the message says which mistake was made:

| You wrote | The message ends |
|---|---|
| a version newer than the build, `meridian/v9` | `it is newer than this build, which understands meridian/v1; upgrade meridian` |
| something that is not a whole number, `meridian/v1.2` | `versions are whole numbers, as in "meridian/v1"` |
| another tool's version, `apps/v1` | `expected "meridian/v1"` |

What does and does not need a new version:

- **Additions never do.** A new optional field, a new kind of document or a new
  resource type arrives inside the current version. A document that compiled
  yesterday compiles the same today.
- **Anything that could change what an existing document does, does.** Removing
  a field, changing what a field means, tightening a check, and changing a
  default, even though a changed default leaves every document textually valid.
- A version is supported for at least 24 months after the next one ships.
- The release that introduces a version ships the
  [`meridian migrate`](cli.md#migrate) step that moves documents to it.

## Plugin protocol

`meridian` and a plugin are separate programs, so they can be different ages.
Each speaks a range of protocol versions, and they agree on one when the plugin
starts:

1. `meridian` starts the plugin with the environment variable
   `MERIDIAN_PLUGIN_PROTOCOLS` set to the versions it speaks, lowest first. This
   build sets it to `2`.
2. The plugin picks the highest version in that list that it also speaks and
   announces it in the first line it prints:
   `MERIDIAN-PLUGIN|<protocol>|<sdk version>|unix|<socket>`.
3. `meridian` checks that the announced version is one it offered, and both
   sides speak that version from then on.

A plugin built before this negotiation existed ignores the variable and
announces its one version, which is accepted when it is in the range. So an
older plugin keeps loading on a newer `meridian` for as long as its protocol
version is still spoken, and nobody has to upgrade both at once.

| Protocol | Added | Spoken by this build |
|---|---|---|
| 1 | `Emitter`: `Describe` and `Emit`. Compile only | no |
| 2 | The plugin's category and operations in `Describe`; the `Runner` service (plan, apply, outputs). Later additions within 2, each ignored by a side that predates it: `Describe` fields up to `settings`; `Runner.Cancel`, which stops a plan or apply (a plugin without it cannot be stopped, and `meridian` says so) | yes |

When there is no version in common the plugin is not loaded, and the message
says which side is behind:

```
the plugin speaks protocol version 1 and this host speaks 2; the plugin is older than this host supports: rebuild it against a current SDK
the plugin speaks protocol version 5 and this host speaks 2; the plugin is newer than this host: upgrade meridian
```

`meridian` speaks at most two protocol versions at a time, next to each other.

## Plugin SDK

The first line a plugin prints also says which SDK release it was built
against. `meridian` loads the plugin when that release is on the same line as
the SDK it ships and not newer than it:

| Plugin built against | `meridian` ships | Loads |
|---|---|---|
| `1.2.0` | `1.2.0` | yes |
| `1.2.0` | `1.4.0` | yes: a newer SDK on the same major still serves an older plugin |
| `1.5.0` | `1.2.0` | no: the plugin may use what this `meridian` does not have. Upgrade `meridian` |
| `1.4.0` | `2.3.0` | no: a different major. Rebuild the plugin |
| `0.1.0` | `0.1.3` | yes |
| `0.1.0` | `0.2.0` | no: before `1.0` the middle number is the breaking one |

The refusal names both versions and says what to do:

```
the plugin was built against SDK 0.2.0 (0.2.x), and this host ships SDK 0.1.0 (0.1.x); the plugin's SDK is newer than this host supports: upgrade meridian
```

The SDK and the protocol are separate numbers on purpose. The SDK gains
helpers far more often than the messages between `meridian` and a plugin
change, so most SDK releases leave the protocol where it is.

## Modules

A [module](modules.md) says which releases of `meridian` it works with in its
manifest's `spec.meridian`, and a release outside that range refuses the module
by name. A module's documents carry their own `apiVersion`, and are read by
the rules above like any other document.

`meridian.lock` carries a format number, `lockVersion`. A newer `meridian`
reads a lockfile written by an older one. An older `meridian` refuses a
lockfile written in a newer format, and says to upgrade.
