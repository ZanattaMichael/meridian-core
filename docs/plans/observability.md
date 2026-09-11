# Observability, Logging & Diagnostics

> Status: **accepted**. Resolves issue #6.
> Amend by pull request against this file.

The testing plan (§8) tests error-message quality, which assumes the content exists and
is good. This document defines what that content is. It covers what Meridian tells a
user at each pipeline stage, how degradations are surfaced, how a fact's provenance is
shown, whether telemetry is collected, and what the orchestrator records about its own
actions.

The governing observation: Meridian's central design choice is to evaluate conditions at
compile time and emit static artifacts. That buys auditability and costs the user the
ability to read a runtime conditional and understand why something happened. The
compile-time trace has to repay that debt, and if it does not, the design choice was a
bad trade.

---

## 1. Structured logging

**Decision: structured JSON to stderr, human-readable text when stderr is a terminal, one
schema across every stage, with a stable resource id as the join key.**

### 1.1 The requirement to meet

A user must be able to follow one resource through `resolve → facts → condition → graph →
transform → sort → validate → emit` and see what happened to it at each step. That means
every log line carries the resource id, so that the trace is a filter rather than an
investigation:

```
$ meridian plan --log-format=json 2>&1 | jq 'select(.resource_id == "nginx_conf")'
```

This is the single most important property of the logging design. Everything below
serves it.

### 1.2 Schema

Every record carries these fields. Stage-specific fields go under `detail` and are never
promoted to the top level, so a consumer can rely on the shape.

| Field | Type | Notes |
|---|---|---|
| `ts` | RFC 3339, UTC | |
| `level` | `debug`/`info`/`warn`/`error` | |
| `stage` | enum | One of the nine pipeline stages, or `orchestrator` |
| `run_id` | ULID | One per `meridian` invocation. Joins across stages and nodes |
| `resource_id` | string | The IR `id`. Absent only for genuinely resource-independent lines |
| `document` | string | `metadata.name` of the owning document |
| `target` | string | Absent before `transform`, which is the first target-aware stage |
| `node` | string | Present from `facts` onward for `ResourceSet` runs |
| `msg` | string | Human-readable, no interpolated values (see §1.4) |
| `detail` | object | Stage-specific, typed per stage |

`stage` being an enum rather than free text matters: it makes "show me everything the
condition stage did" a reliable query, which is how most debugging sessions start.

### 1.3 Levels, defined by audience rather than by severity

Severity-based level definitions produce logs where `info` means whatever each author
felt. These are defined by who the line is for:

- **`error`** — compilation or apply failed. Always actionable by the user. Always names
  the resource and the rule.
- **`warn`** — succeeded, but the user is getting something other than the obvious
  reading of their IR. Every capability degradation is a `warn` (§3).
- **`info`** — stage boundaries, resource counts, decisions a user would want in a CI log
  they read six months later.
- **`debug`** — per-resource decisions: each merge layer applied, each pruning outcome,
  each sort tiebreak. Off by default, and the thing to turn on first.

A useful test of the boundary: if a line would appear in a green CI run and nobody would
ever read it, it is `debug`, not `info`.

### 1.4 Values never go in `msg`

`msg` is a constant string. Values go in `detail`. This keeps messages greppable across
runs, and it is the structural reason a secret cannot leak through a format string: there
is no format string. It complements the type-level guarantee in
[the security model §2](security.md#2-what-secret-true-guarantees-per-stage) rather than
duplicating it, since two independent mechanisms are what makes the property hold when
someone adds a logger without reading this document.

### 1.5 Per-stage `detail` contracts

Each stage's `detail` shape is defined and tested. The high-value ones:

| Stage | `detail` carries |
|---|---|
| `resolve` | `key`, `strategy`, `layers_consulted[]`, `winning_layer` |
| `facts` | `provider`, `fact`, `outcome`, `duration_ms` |
| `condition` | `expression`, `operands[]` (names, never values), `outcome`, `pruned` |
| `graph` | `edge_kind`, `from`, `to`, `tiebreak_index` |
| `transform` | `ir_type`, `native_type`, `unmapped_params[]` |
| `sort` | `position`, `parallelism_preserved` |
| `emit` | `file`, `bytes`, `sha256` |

`resolve.winning_layer` and `graph.tiebreak_index` are the two that will matter most in
practice. They answer "where did this value come from" and "why is this task before that
one," which are the two questions a static-output model gets asked most.

---

## 2. Fact provenance

Design plan §6.1 keeps `facts.*` in its own namespace specifically for provenance and
debuggability. That intent needs a user-facing surface or it is just an internal
convention.

**Decision: `meridian explain <resource-id>` — a dedicated subcommand, not a flag on
`plan`.**

A flag would have to interleave per-resource provenance into whole-run output, which is
unreadable at any real resource count. `explain` answers one question about one resource
and can be shaped for exactly that:

```
$ meridian explain nginx_conf --node web-01

nginx_conf  (type: file)  document: web-server-baseline  target: ansible

INCLUDED — `when` evaluated true
  expression: facts.os_release.family == 'debian' and nginx_enabled
  facts.os_release.family = "debian"
      from: fact provider `os_release`, gathered from web-01 at 09:14:22Z (4m ago)
  nginx_enabled = true
      from: hierarchy layer `role/webserver.yaml`
      shadowed: common.yaml had `false` (strategy: first-found)

PARAMS
  path   = /etc/nginx/nginx.conf     literal, from IR
  source = templates/nginx.conf.j2   literal, from IR
  owner  = "www-data"                from hierarchy layer `environment/prod.yaml`

EDGES
  dependsOn  nginx_pkg   (hard order, ifMissing: error)
  notifies   nginx_svc   on: changed, action: restart
      target support: ansible — native (handler, deferred to end of play)

POSITION
  sorted to index 7 of 12
  tiebreak: declaration order (no edge constrains this against `logrotate_conf`)
```

The `shadowed:` line is the part that earns the feature. "Which layer won" is answerable
by reading the hierarchy; "which layers lost, and what they said" is what a user actually
needs when a value is not what they expected, and it is expensive to reconstruct by hand.

Provenance is tracked through the pipeline whenever `explain` is possible, which means
the resolver carries layer attribution alongside every value rather than reconstructing
it afterwards. Reconstruction would re-run the merge and could disagree with what
actually happened, which is the one thing a provenance feature must never do.

Secret values render as `***` here as everywhere, but their **provenance is still shown** —
which layer a secret came from is not itself sensitive, and withholding it would make
secrets the hardest values to debug precisely when getting them wrong is most costly.

---

## 3. Plan output

Design plan §6.3, §7 and §8 each specify a degradation notice separately: staleness
warnings, flattened parallelism, synthetic notify. Three specifications written in three
places will produce three formats. This unifies them.

**Decision: one notice format, rendered inline at the point of relevance, and summarized
at the end. Degradations are never only in verbose logs.**

### 3.1 Format

Every degradation notice has four parts: what was asked for, what will actually happen,
what it costs, and where to read more.

```
~ nginx_svc  notify from nginx_conf
    requested: restart on change
    actual:    synthetic — Script resource with hash-file change detection
    cost:      adds /var/lib/meridian/dsc/nginx_svc.hash on the target node
    docs:      /targets/dsc-classic#synthetic-notify
```

The `cost:` line is the one that matters and the one most likely to be dropped under
pressure. "Supported via a workaround" is not informative; "this creates a state file on
your node" is. The documentation plan (§3.2) makes the same argument for the prose docs,
and this is the same principle applied to tool output.

### 3.2 Markers

| Marker | Meaning |
|---|---|
| `+` | Resource will be created |
| `~` | Degradation: supported, but not the way the IR reads |
| `-` | Resource pruned by `when` |
| `!` | Warning that is not a degradation: stale facts, unguarded `exec` |
| `x` | Hard failure. Plan does not complete |

### 3.3 Summary block

Inline notices are missed in a long plan, so the run ends with a count:

```
Plan: 12 resources, 2 pruned, 3 degradations, 1 warning.

DEGRADATIONS (3)
  ~ nginx_svc     notify   synthetic (hash file on node)     /targets/dsc-classic#synthetic-notify
  ~ web_vm        notify   forced replacement (destroy/create) /targets/terraform-arm#notify-replacement
  ~ <all>         order    parallelism flattened to sequence  /targets/ansible#flattening

WARNINGS (1)
  ! facts for web-01 gathered 47m ago (threshold 30m) — re-run with --refresh-facts
```

`--no-degradation-summary` exists for scripted use. There is deliberately no flag that
suppresses the inline notices, because a user who has hidden the degradations is in
exactly the position the design plan's no-silent-capability-loss principle exists to
prevent.

### 3.4 Staleness threshold

Design plan §13 leaves this open. **Decision: default 30 minutes, configurable via
`facts.staleness_threshold`, and `meridian apply` refuses to proceed past 4× the
threshold without `--refresh-facts`.**

Thirty minutes is short enough that a plan reviewed and applied in one sitting never
trips it, and long enough to survive a slow code review. The hard stop at 2 hours exists
because a day-old plan applied against a changed host is the specific failure mode that
the gather-and-prune model introduces and that runtime conditionals would not have had.

---

## 4. Telemetry

**Decision: Meridian collects no telemetry. No usage data, no anonymous metrics, no
version-check phone-home. There is no opt-out flag because there is nothing to opt out
of.**

This is a positioning decision as much as a privacy one. Meridian is a build-time tool
that runs on infrastructure control nodes and CI runners, handling credentials in transit
and compiling artifacts that describe a customer's production estate. The population most
likely to adopt it is the population most likely to run it in a network where an outbound
connection from a build tool is a finding in an audit.

The counter-argument is real: without telemetry the project cannot tell which targets are
used, which resource types matter, or which degradations people hit. Accepted. The
substitutes are voluntary and coarse: release download counts, issue and discussion
volume, and direct conversation with adopters. Worse data, and worth it.

Two consequences, so this is not quietly re-litigated later:

- **No version check.** `meridian version` reports what is installed, and does not ask a
  server whether it is current.
- **Adding telemetry later is a breaking change of trust**, not a feature. It requires a
  major version and prominent release-note treatment, per
  [release and versioning](release-and-versioning.md#6-deprecation-policy).

Meridian writes diagnostics to local disk only, under the run root, and never transmits
them. `meridian bug-report` bundles a redacted run for a user to attach to an issue
**themselves**; it uploads nothing.

---

## 5. Orchestrator audit log

Design plan §11.5 gives the orchestrator its own run-state. Run-state answers "what is
the current state"; an audit log answers "who changed what, when, and against which
node." These are different questions and one store answering both answers neither well.

**Decision: an append-only audit log, separate from run-state, written locally in JSON
Lines.**

### 5.1 Record

```json
{
  "ts": "2026-09-11T09:14:22Z",
  "run_id": "01JBX...",
  "actor": {"user": "ci-runner", "host": "build-07", "source": "ci"},
  "action": "resourceset.apply",
  "document": "web-server-baseline",
  "target": "ansible",
  "node": "web-01",
  "artifact_sha256": "9f2c...",
  "meridian_version": "0.4.2",
  "plugin_version": "ansible/0.4.1",
  "outcome": "success",
  "changed_resources": ["nginx_conf", "nginx_svc"],
  "duration_ms": 18342
}
```

`artifact_sha256` is what makes the log evidential rather than anecdotal. Because emitted
artifacts are deterministic (design plan §2.6) and contain no conditionals, the hash
identifies exactly what was applied, and anyone can recompile the same IR and compare.
That property is worth more than any amount of descriptive logging, and it exists only
because determinism was a design requirement.

`actor` is best-effort: OS user, hostname, and whether the run looks like CI. Meridian has
no identity system and will not grow one. The documentation says this plainly rather than
letting the field imply authentication it does not perform.

### 5.2 Properties

- **Append-only.** Meridian never rewrites or truncates it. Rotation is the operator's,
  via ordinary log tooling.
- **Written before and after.** An `intent` record precedes the apply and an `outcome`
  record follows it. A crash mid-apply then leaves an intent with no outcome, which is
  the state most worth being able to see.
- **Local by default.** Configurable path; optional stream to a file descriptor for
  operators shipping to a log system. Meridian ships nothing anywhere itself (§4).
- **No secrets, ever.** Same rule and same mechanism as
  [the security model §4](security.md#4-the-run-state-store). `changed_resources` lists
  ids, never values.
- **Failures are recorded, including refusals.** A stale-plan refusal (§3.4) is an audit
  record. "Why did the deploy not run" is a question the log must be able to answer.

### 5.3 What it is not

Not a compliance product, not tamper-evident, not signed. A local append-only file is
modifiable by anyone who can write to it. An operator needing tamper-evidence ships the
stream to a system that provides it. Claiming more than the format delivers would make
the log worse than not having one, because someone would rely on it.
