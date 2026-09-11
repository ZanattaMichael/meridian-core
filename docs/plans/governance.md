# Governance & Contribution Model

> Status: **accepted**. Resolves issue #4.
> Amend by pull request against this file.

The plugin SDK makes third-party target plugins a first-class scenario rather than an
afterthought. That creates a process gap: someone will offer a `meridian-saltstack`
plugin, and the project needs an answer to "is this supported?" that is decided in
advance rather than negotiated in the pull request. This document is that answer.

The governing constraint: Meridian's whole value proposition is that a capability claim
can be trusted. A governance model that lets unverified claims into the capability
matrix destroys the product, not just the process.

---

## 1. Plugin tiers

**Decision: three tiers, and the tier is a property of the plugin, not of who wrote it.**

A plugin is listed at exactly one tier. The tier appears in the generated capability
matrix, in `meridian plan` output, and on the target's documentation page. A user never
has to dig to find out which tier they are relying on.

| Tier | Test bar | Who maintains it | Ships in |
|---|---|---|---|
| **Supported** | Tier 1–4 all green, including containerized integration tests | Core team commits to keeping it green | The `meridian` binary |
| **Community** | Tier 1–3 green. Tier 4 not required | The contributing author, named in the plugin manifest | Installed separately |
| **Experimental** | Tier 1–2 green, Tier 3 may have declared exemptions | Anyone | Installed separately, warns on every run |

### 1.1 The contribution bar, precisely

The issue asks whether the full Tier 2/3 suite must pass before **merge** or before
being listed as **supported**. These are different gates and the answer differs.

**Before merge: Tier 2 and Tier 3 must pass.** No exceptions, at any tier, including
experimental. Tier 3 is the cross-plugin contract suite — it is the mechanism that proves
a capability claim means the same thing on this target as on every other one. A plugin
that has not passed it has made claims the project cannot stand behind, and merging it
puts an unverified row in the capability matrix. That is the specific failure this whole
model exists to prevent.

An experimental plugin may declare **exemptions** from individual Tier 3 fixtures, but an
exemption is explicit, listed in the plugin manifest, and rendered in the capability
matrix as "not verified" rather than being silently absent. An unverified cell and a
false cell are both bad; an invisible cell is worse than either.

**Before listing as Supported: Tier 4 must pass in CI.** Containerized integration tests
against the real target CLI. This is the gate that separates "the emitted artifact looks
right" from "the emitted artifact does the right thing," and it is exactly the gap the
testing plan (§1.5) identifies as the one golden files cannot close.

### 1.2 Promotion and demotion

Promotion Experimental → Community → Supported requires a core-team decision, recorded
in the pull request that changes the tier.

Demotion is not a punishment and is not personal. A Supported plugin whose Tier 4 job has
been red for **30 days** is demoted to Community automatically. A Community plugin whose
maintainer has not responded to a capability-affecting issue in **90 days** is demoted to
Experimental. Both are announced in release notes.

Automatic demotion exists because the alternative is a matrix where "Supported" gradually
comes to mean "was supported once." A tier that is never revoked is not a tier.

---

## 2. Ownership model

**Decision: yes, first-party plugins carry core-team ownership, and the lighter process
for community plugins is real, not nominal.**

First-party plugins — `ansible`, `puppet`, `terraform`, `dsc-classic`, `dsc-v3`, `chef` —
are Supported tier, live in `meridian-core` until the multi-repo split (§5), and are
owned by the core team. Core-team ownership means the core team is on the hook for
keeping them green; it does not mean only the core team may change them.

Community plugins live in `meridian-<target>` repositories under the project
organization, are owned by their listed maintainer, and get a deliberately lighter
process: one maintainer approval to merge, no core-team review required for changes that
do not touch capability declarations or secrets handling. Those two carve-outs are in
§3 and §5.

The asymmetry is intentional. A first-party plugin's bugs are the project's
responsibility because the project ships them in its binary. A community plugin's bugs
are its maintainer's, and imposing core-team review on every change would make community
maintainership unattractive without making the plugin better.

**Design plan §2.5 stays load-bearing.** First-party plugins are built against the same
public SDK a third party uses, with no privileged access to `internal/`. Testing plan §8
enforces this with a reference plugin built purely against `pkg/sdk`. Ownership tiers
describe maintenance commitment. They must never become a technical privilege, because
the moment a first-party plugin reaches into `internal/`, the SDK stops being the thing
third parties can actually build against.

---

## 3. Capability-matrix disputes

A contested `Capabilities{}` claim is the highest-stakes disagreement this project can
have, because the matrix is the thing users make decisions on.

**Decision: disputes are resolved by evidence first, and only by authority when evidence
cannot settle it.**

The process, in order:

1. **The claim must be falsifiable.** A reviewer contesting `NativeNotify: true` writes a
   test fixture that would fail if the claim were false. A dispute that cannot be
   expressed as a test is a documentation disagreement, not a capability dispute, and is
   handled as an ordinary review comment.
2. **The test decides.** If the fixture passes, the claim stands. If it fails, the claim
   is withdrawn or downgraded to `SyntheticNotify`. The fixture is merged either way — it
   is now a regression guard, and the dispute has made the suite stronger.
3. **Genuine ambiguity escalates.** Some disputes are not about facts but about
   definitions: is Chef's `:immediately` notify timing the "same" capability as Ansible's
   deferred handler? Those go to the core team, which decides by majority, and the
   decision is written into `docs/concepts/capability-model.md` as a definition, not left
   as a one-off ruling.
4. **Ties break toward the weaker claim.** A deadlocked core team downgrades. An
   overstated capability produces a user whose production deployment silently did not do
   what they asked. An understated one produces a user who used an escape hatch they did
   not strictly need. The second failure is recoverable and the first is not, so the tie
   does not break in the middle.

Rule 4 is the substantive decision here. Everything above it is procedure.

---

## 4. Security review requirement

Per [the security model §6](security.md#6-plugin-sdk-secrets-contract), plugins handle
secrets-adjacent data and a false `SecretsAtRest` claim is a security bug rather than a
documentation bug.

**Decision: a second reviewer is required for any change touching the secrets contract,
at every tier including Experimental.**

Specifically, a change requires security review when it:

- Adds or modifies a `SecretsBackend` or `SecretsAtRest` declaration.
- Adds or moves a call to `Reveal()`. The security model allows exactly one call site per
  plugin; a diff that adds a second is rejected on sight, not reviewed on merit.
- Touches `exec` escaping or any path that builds a shell command string.
- Changes what reaches Meridian's run-state store.

The reviewer is a core-team member for a first-party plugin, or any core-team member for
a community or experimental one. Community plugins get a lighter process everywhere else
(§2) and deliberately not here: an experimental plugin leaking a credential harms the
user exactly as much as a supported one doing it, and the "experimental" label is not
informed consent to that.

New plugin authors are pointed at `docs/sdk/capabilities-contract.md` before their first
pull request, so the obligations are known before the code is written.

---

## 5. Triage, and the multi-repo split

### 5.1 Issue and pull request triage

Every incoming issue gets one of four labels within **5 working days**:

| Label | Meaning | Response commitment |
|---|---|---|
| `core` | Pipeline stages, IR, SDK | Core team owns it |
| `plugin/<target>` | A specific target | Routed to that plugin's owner |
| `capability-dispute` | Contested `Capabilities{}` claim | §3 process, core team notified |
| `docs` | Documentation only | Any maintainer |

Security reports do not enter this flow at all. They go through private advisories under
[the security model §7](security.md#7-vulnerability-disclosure), which has its own,
shorter clock.

A capability-dispute issue against a Supported plugin is treated as a **potential user
harm**, not a feature request, and is triaged at the same urgency as a bug, because the
matrix being wrong is the one failure that undermines every other guarantee.

An issue with no maintainer response in 60 days is closed as stale, with a note saying
reopening is welcome. Stale-closing is a statement about the project's capacity, not
about the issue's merit, and the note should say so.

### 5.2 Split timing

The issue asks whether the multi-repo plugin org split happens before or after this
governance model is formalized.

**Decision: this governance model comes first, and it is a precondition for the split.**

Design plan §12 already gates the split on SDK stability — 2 to 3 plugins built and the
boundary proven. This adds a second gate, and the ordering is the point: a split
distributes plugins across repositories with different reviewers and different merge
buttons. Doing that before there is an agreed contribution bar, an agreed tier
definition, and an agreed dispute process means each repository invents its own, and
they will not match. Consolidating divergent processes afterwards is much harder than
agreeing one upfront.

So the split proceeds when **both** hold:

1. The SDK boundary is stable (design plan §12): 2–3 plugins built against it, and the
   reference-plugin test from testing plan §8 passing with no `internal/` leakage.
2. This document is accepted and has survived at least one real third-party plugin
   contribution end to end. A governance model that has never been exercised is a draft,
   whatever its status header says.

Until both hold, everything stays in `meridian-core`.
