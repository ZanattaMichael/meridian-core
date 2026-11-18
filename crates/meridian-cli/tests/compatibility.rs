//! The compatibility page in the documentation is generated from the version
//! constants in the code, and this test fails when the two differ. Record it
//! again with `UPDATE_GOLDEN=1` after a version changes on purpose.

use std::fs;

use meridian_ir::{Status, VersionTable};
use meridian_plugin_testkit::repo_root;
use meridian_sdk::version::{describe_protocols, offer, supported_protocols, PROTOCOLS_ENV};
use meridian_sdk::{PROTOCOL_VERSION, VERSION};

const PATH: &str = "docs/reference/compatibility.md";

/// What each protocol version added. A new version gets a row here, and the
/// test below fails until it does.
const PROTOCOL_HISTORY: &[(u32, &str)] = &[
    (
        1,
        "`Emitter`: `Describe` and `Emit`. Compile only",
    ),
    (
        2,
        "The plugin's category and operations in `Describe`; the `Runner` service (plan, apply, outputs). Later additions within 2, each ignored by a side that predates it: `Describe` fields up to `settings_keys` (a plugin without `settings_keys` takes no `spec.settings`, and a document that sets any is refused before it is asked); `ResourceGraph.settings_json`; `Runner.Cancel`, which stops a plan or apply (a plugin without it cannot be stopped, and `meridian` says so); `Describe.saves_plans`, `RunContext.save_plan` and `RunContext.saved_plan`, and `PlanResult.saved`, for a [saved plan](../operations/pull-requests.md) (a plugin without them is checked by its artifact hash and plans again at apply); `Resource.adopt`, `Capabilities.adopt` and the `adopt` change action (a plugin without them reports `none`, and `adopt` is refused before it is asked); `Describe.reads_identifiers` and `Runner.Identifiers`, for [`meridian retarget`](../operations/retargeting.md) (a plugin without them cannot be the old target of a move that adopts)",
    ),
];

fn page() -> String {
    let table = VersionTable::builtin();
    let range = supported_protocols();
    let core = env!("CARGO_PKG_VERSION");
    let mut out = String::from(
        "\
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

",
    );
    out.push_str("| | |\n|---|---|\n");
    out.push_str(&format!("| Core | `{core}` |\n"));
    out.push_str(&format!("| Language versions read | {} |\n", {
        let names: Vec<String> = table
            .versions()
            .iter()
            .filter(|v| !matches!(v.status, Status::Removed { .. }))
            .map(|v| format!("`{}`", v.name()))
            .collect();
        names.join(", ")
    }));
    out.push_str(&format!("| Plugin SDK shipped | `{VERSION}` |\n"));
    out.push_str(&format!(
        "| Plugin protocol versions spoken | {} |\n",
        describe_protocols(&range)
    ));
    out.push_str(&format!(
        "| [Lockfile](modules.md#the-lockfile) format written | {} |\n",
        meridian_module::LOCK_VERSION
    ));

    out.push_str(
        "
## Language versions

Every document says which version of the language it is written in. The
version is looked up before anything else in the document is read.

| `apiVersion` | In this build | What happens to a document written in it |
|---|---|---|
",
    );
    for v in table.versions() {
        let (state, effect) = match v.status {
            Status::Supported => ("supported".to_owned(), "It compiles".to_owned()),
            Status::Deprecated { since } => (
                format!("deprecated since {since}"),
                "It compiles, and the run warns once. Move it with [`meridian migrate`](cli.md#migrate)".to_owned(),
            ),
            Status::Removed { in_release } => (
                format!("removed in {in_release}"),
                "It is refused. Move it with [`meridian migrate`](cli.md#migrate)".to_owned(),
            ),
        };
        out.push_str(&format!("| `{}` | {state} | {effect} |\n", v.name()));
    }
    out.push_str(
        "
Anything else is refused, and the message says which mistake was made:

| You wrote | The message ends |
|---|---|
| a version newer than the build, `meridian/v9` | `it is newer than this build, which understands meridian/v1; upgrade meridian` |
| something that is not a whole number, `meridian/v1.2` | `versions are whole numbers, as in \"meridian/v1\"` |
| another tool's version, `apps/v1` | `expected \"meridian/v1\"` |

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
   `",
    );
    out.push_str(PROTOCOLS_ENV);
    out.push_str(&format!(
        "` set to the versions it speaks, lowest first. This
   build sets it to `{}`.
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
",
        offer(&range)
    ));
    for (version, added) in PROTOCOL_HISTORY {
        let spoken = if range.contains(version) { "yes" } else { "no" };
        out.push_str(&format!("| {version} | {added} | {spoken} |\n"));
    }
    out.push_str(
        "
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
",
    );
    out
}

#[test]
fn the_documented_compatibility_is_what_the_code_does() {
    assert!(
        PROTOCOL_HISTORY.iter().any(|(v, _)| *v == PROTOCOL_VERSION),
        "protocol version {PROTOCOL_VERSION} has no row in PROTOCOL_HISTORY saying what it added"
    );
    let path = repo_root().join(PATH);
    let generated = page();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with the version constants; run the tests with UPDATE_GOLDEN=1 to record it again"
    );
}
