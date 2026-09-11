# Release, Versioning & Compatibility Policy

> Status: **accepted**. Resolves issue #7.
> Amend by pull request against this file.

Meridian is not one thing that can carry one version number. It is four things that
evolve on different clocks: the IR schema that users author against, the plugin SDK that
third parties compile against, individual plugins, and the orchestrator's run-state
schema on disk. A single version number across all four forces a core release for a
plugin bugfix and says nothing useful about compatibility.

---

## 1. Four version streams

**Decision: four independently versioned surfaces, each with its own compatibility
promise.**

| Surface | Scheme | Promise |
|---|---|---|
| IR / `apiVersion` | `meridian/v1`, `v2` — integers, not semver | A document valid under `v1` compiles under every release supporting `v1` |
| Core binary | SemVer `MAJOR.MINOR.PATCH` | Standard SemVer on CLI behavior and exit codes |
| Plugin SDK | SemVer, versioned separately from core | A plugin built against SDK `1.x` loads on any core shipping SDK `1.y`, `y >= x` |
| Run-state schema | Monotonic integer | Readable N and N-1; migrated forward, never backward |

The IR uses integers rather than semver deliberately. `apiVersion` appears in every
document a user writes, and `meridian/v1.3.0` invites them to wonder whether their
document needs updating on every minor release. It never does. An integer says the only
thing the field needs to say.

---

## 2. IR `apiVersion`

### 2.1 What signals what

**Additive changes do not bump `apiVersion`.** A new optional field, a new resource type,
a new merge strategy — all land within `meridian/v1`. Existing documents keep compiling
identically. Testing plan §3.1 requires an unknown *future* `apiVersion` to be an explicit
"unsupported apiVersion" error; unknown *fields* within a supported version are also an
error, because silently ignoring a misspelled field is how a user ends up with a resource
that quietly does not do what they wrote.

**Breaking changes bump the integer.** Removing a field, changing a default, changing the
meaning of an existing field, or tightening validation so a previously valid document
fails.

Changing a **default** is explicitly breaking, even though nothing in the document
changes. `dependsOn[].ifMissing` defaults to `error` (design plan §6.4); flipping it to
`skip` would change what existing documents do while they remain textually valid. Those
are the most dangerous changes to make, so they are classified with the most friction.

### 2.2 Support window

**Decision: an `apiVersion` is supported for 24 months after its successor ships, and for
at least one full major release of the core binary, whichever is longer.**

Twenty-four months is chosen against the adoption reality in
[migration and adoption](migration-and-adoption.md#4-pilot-adoption-guidance): teams adopt
incrementally, in production, alongside existing configuration. An estate that took a year
to migrate cannot be asked to re-migrate on a twelve-month clock. The "at least one major
release" floor stops a fast release cadence from compressing the window below what the
calendar promised.

During the window both versions compile. A deprecated `apiVersion` produces one `warn` per
run, not per document — a thousand-document estate should not generate a thousand
warnings, because that just teaches people to filter them.

### 2.3 Migration tooling is a precondition, not a follow-up

`meridian migrate --to=v2` ships **in the same release** that introduces `v2`. Not later.
A breaking change without mechanical migration transfers the project's cost onto every
user, multiplied by their document count.

Where migration cannot be mechanical, the tool says so per document and points at the
specific decision needed. It never guesses. This is the same principle as the refusal to
build a reverse compiler ([migration and adoption §1](migration-and-adoption.md#1-scope-decision-forward-compiling-only)):
an approximate transformation of production configuration is worse than an honest refusal.

---

## 3. Plugin SDK compatibility

### 3.1 The promise

**A plugin built against SDK `1.x` runs on any core shipping SDK `1.y` where `y >= x`.
Forward compatibility is not promised: a plugin built against `1.5` does not load on a
core shipping `1.2`.**

This is ordinary SemVer, stated explicitly because it answers the issue's question
directly: **a plugin built against SDK v1 does not run against core shipping SDK v2.** The
plugin loader refuses it by version check rather than letting it fail at an arbitrary
call site, so the failure names the cause:

```
x plugin `meridian-saltstack` requires SDK 1.x, this build ships SDK 2.3.
    The plugin needs rebuilding against SDK 2.x.
    docs: /sdk/plugin-architecture#sdk-compatibility
```

The SDK version is a compiled-in constant reported over the gRPC handshake, not inferred
from the plugin binary. Inference would be wrong exactly when it matters.

### 3.2 Deprecation window

**Decision: an SDK major version is supported for 18 months after its successor ships, and
core supports loading exactly two adjacent SDK majors at once.**

Eighteen months rather than the IR's 24: the affected population is plugin authors, who
are far fewer than IR authors and are already engaged with the project's release cycle.
Two adjacent majors is the maximum, because supporting three means the core carries three
sets of adapter code, which is how an SDK boundary rots.

An SDK major bump requires a written migration guide covering every changed interface,
published with the release, and a mechanical rewrite where one is possible.

### 3.3 The stability bar for the multi-repo split

Design plan §12 makes the multi-repo plugin split contingent on SDK stability without
defining it. This defines it.

The SDK is **stable** — and eligible to be versioned `1.0` — when all of these hold:

1. Three plugins are built against it, including at least one structurally unlike the
   others. `ansible`, `puppet` and `terraform` qualify: push, catalog, and plan/apply.
   Three variations on one execution model would not.
2. The reference-plugin test (testing plan §8) passes with no `internal/` access,
   proving a third party can build a working plugin from the public surface alone.
3. Two consecutive releases have shipped with no breaking SDK change.
4. One plugin has been built against it by someone outside the core team, end to end,
   without an SDK change being needed to accommodate them.

Item 4 is the real bar. The first three can all be satisfied by a team testing its own
assumptions. Only an outsider finds the place where the SDK assumed something nobody
wrote down.

[Governance §5.2](governance.md#52-split-timing) adds a second, independent precondition:
the governance model must itself have been exercised. Both must hold.

---

## 4. Run-state schema migration

Design plan §11.5 puts run-state on disk, where it outlives the binary that wrote it.
Testing plan §7 calls for an N/N-1 upgrade test; this is the policy that test verifies.

**Decision: a monotonic integer schema version, forward migration on read, N and N-1
readable, and never a backward migration.**

| Rule | Detail |
|---|---|
| Version stamp | Every run-state file carries `schema_version` as its first field |
| Forward migration | A newer binary reading older state migrates it in place, after writing a `.bak` alongside |
| Read window | Version N reads N and N-1. Older than N-1 is an error naming the intermediate release to upgrade through |
| No backward migration | An older binary reading newer state refuses. It does not attempt a best-effort read |
| Atomic write | Write to a temporary file, `fsync`, rename. A crash mid-write leaves the previous state intact, never a truncated one |

The refusal to migrate backward is the decision worth defending. A rollback to an older
Meridian after an upgrade is a plausible operational move, and supporting it would mean
either discarding fields the newer version added or guessing at their prior values. Both
produce state that describes infrastructure incorrectly, which is the one thing run-state
must never do. Refusing is recoverable — the `.bak` from the forward migration is right
there — and the error says so:

```
x run-state at .meridian/state.json is schema 7; this build reads up to 6.
    It was written by a newer Meridian. Restore .meridian/state.json.bak
    (schema 6, written 2026-09-11T09:14:22Z) or upgrade this binary.
```

Run-state contains no secrets, per
[the security model §4](security.md#4-the-run-state-store), so migration never handles
sensitive values and backup files carry no additional exposure.

---

## 5. Per-plugin versioning and release channels

### 5.1 Independent plugin versions

**Decision: each plugin carries its own SemVer, moving independently of the core binary.**

A bugfix in the Puppet emitter releases as `puppet/0.4.2` without a core release. A core
release does not renumber plugins that did not change. `meridian version` reports the
full set, because "which version of Meridian" is not answerable by one number:

```
$ meridian version
meridian 1.2.0   (sdk 1.4, ir meridian/v1, run-state schema 6)
plugins:
  ansible      1.1.3   supported     sdk 1.4
  puppet       1.0.7   supported     sdk 1.4
  terraform    0.9.1   supported     sdk 1.3
  dsc-classic  0.6.2   supported     sdk 1.3
  dsc-v3       0.3.0   experimental  sdk 1.4
  chef         0.8.4   community     sdk 1.2
```

Tier comes from [governance §1](governance.md#1-plugin-tiers) and is printed here so a
user never has to look it up to know what they are relying on.

### 5.2 Channels

**Decision: two channels, `stable` and `beta`. No nightly.**

`stable` is the default: every Supported plugin green across Tiers 1–4, release notes,
migration guides where needed. `beta` is a pre-release of the next `stable`, for people
who want to test against it, with no compatibility promise between betas.

No nightly channel. A nightly build of a tool that compiles production infrastructure
configuration invites use it should not have, and the maintenance is not free. Anyone
wanting the tip can build from source, which is a proportionate amount of friction.

### 5.3 Gating DSC v3

Testing plan §7 flags DSC v3 as higher-risk and less precedented. Governance §1 defines
the tiers. Combining them:

DSC v3 ships **Experimental** and is promoted to **Supported** only when:

1. Tier 4 containerized integration tests pass against a real `dsc` v3 binary on both
   Windows and Linux targets. Both, because the value of a Rust-rewritten cross-platform
   `dsc` is the cross-platform part.
2. Testing plan §5.4 divergence tests confirm — not assume — whether DSC v3 has a native
   change-detection primitive. Design plan §13 leaves this open. The answer must be
   established by test, and whichever way it lands, it is encoded in the capability
   matrix rather than inherited from DSC classic.
3. A Tier 5 real-target run has passed against non-containerized DSC v3.
4. It has been Experimental through at least two `stable` releases.

Point 2 is the substantive gate. The temptation is to assume DSC v3 inherits DSC classic's
hash-file workaround. Design plan §10 warns against exactly that, and shipping a
`SyntheticNotify` claim that turns out to be unnecessary would be a wrong capability
matrix — the specific failure this project cares most about avoiding.

---

## 6. Deprecation policy

One policy for every deprecable surface: IR fields, CLI flags, SDK interfaces, capability
declarations.

**Decision: a deprecation is a three-stage process, and the stages are measured in
releases, not in calendar time.**

| Stage | Behavior | Minimum duration |
|---|---|---|
| 1. Announced | Works unchanged. Documented as deprecated with a named replacement | 1 minor release |
| 2. Warning | Works unchanged. One `warn` per run naming the replacement | 2 minor releases, or the §2.2 window for IR fields |
| 3. Removed | Hard error naming what was removed, when, and what replaces it | — |

A deprecation never skips a stage, and stage 3 requires a major version bump on the
affected surface.

**A removed field's error message is part of the removal.** A user upgrading across the
window gets `unknown field 'runtimeWhen'` from a generic validator unless the removal is
done properly. The validator retains knowledge of removed fields for one major version
so it can say:

```
x resource `nginx_svc`: field `runtimeWhen` was removed in meridian/v2.
    Replaced by `when` with a `facts.*` expression (removed 2027-03, announced 1.4).
    docs: /reference/dsl/when-and-runtimewhen#migrating-from-runtimewhen
```

Two named cases from the issue:

- **A changed capability declaration** — a target losing `NativeNotify` because the
  underlying tool changed — follows the same three stages and is announced in release
  notes under a dedicated heading. It is not a silent matrix edit. Per
  [governance §3](governance.md#3-capability-matrix-disputes) the change must be
  accompanied by the test that demonstrates it.
- **`runtimeWhen` usage patterns changing** is the case the escape hatch most likely faces.
  Because it is an escape hatch, a shrinking population uses it and a removal is tempting.
  It still goes through all three stages. Users of an escape hatch are, by definition, the
  users with the least alternative.

Adding telemetry would also be governed by this policy, per
[observability §4](observability.md#4-telemetry), though it is a trust change rather than
an API change and requires a core major version regardless of stage timing.
