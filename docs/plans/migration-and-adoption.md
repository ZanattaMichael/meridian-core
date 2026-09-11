# Migration & Adoption Path for Existing Configurations

> Status: **accepted**. Resolves issue #5.
> Amend by pull request against this file.

Nobody adopts Meridian on a greenfield estate. Every team evaluating it already has
Ansible playbooks, Puppet manifests, or Terraform HCL in production, and the honest
question they are asking is not "is this a good design" but "what does the first month
cost me." A tool that can only be adopted by rewriting everything first will not be
adopted.

---

## 1. Scope decision: forward-compiling only

**Decision: Meridian is strictly forward-compiling. Reverse-compilation of native
configuration into Meridian IR is out of scope, permanently, not deferred.**

This is the largest scope decision in the project and it is made deliberately here, not
left to default.

### 1.1 Why not

Reverse-compilation sounds like a one-time import problem. It is not; it is a permanent
correctness problem, for four reasons:

- **It inverts the project's core guarantee.** Design plan §2 evaluates all conditions at
  compile time and emits fully static artifacts. An existing playbook is full of runtime
  `when:` clauses whose truth depends on facts from a host that may not exist right now.
  There is no correct way to turn a runtime conditional into a compile-time one without
  knowing the facts, and inventing them would produce IR that is wrong in a way no test
  catches.
- **Native config is more expressive than the IR, by design.** The IR is deliberately a
  common subset across six tools. Ansible loops, Jinja templating, Puppet's defined types
  and custom functions, Chef's arbitrary Ruby have no IR representation. A reverse
  compiler meets these constantly and its only options are to fail or to emit `exec`
  blocks. A converter that emits `exec` for a third of a playbook has not converted it,
  and has produced something worse than the original: less readable, and now carrying a
  false claim of portability.
- **Round-tripping is the real expectation, and it cannot be met.** A team that is given
  an importer will expect `native → IR → native` to be an identity. It will not be. It
  will reorder tasks per the deterministic topological sort, drop comments, and
  restructure conditionals. Every one of those diffs is a support conversation, and the
  honest answer to all of them is "the importer is approximate," which is not an answer
  anyone accepts about their production configuration.
- **It would consume the roadmap.** Six targets means six importers, each needing a full
  parser for a language the project does not control and which changes under it. That is
  comparable in size to the entire forward path, spent on a one-time migration step
  rather than on the thing the project is for.

### 1.2 What replaces it

Three things, in order of how most teams will actually use them:

1. Coexistence, so Meridian does not have to own everything to own something (§2).
2. The `exec` escape hatch as a deliberate on-ramp (§3).
3. A recommended pilot path that starts where the abstraction is genuinely strongest (§4).

### 1.3 The one carve-out

A **read-only advisory linter** is in scope later, and is explicitly not an importer. It
reads existing native configuration and reports what a Meridian equivalent would and
would not be able to express:

```
$ meridian assess ./playbooks/webserver.yml
  34 tasks examined
  27 map to IR resource types directly
   4 use runtime `when:` on facts — see concepts/conditions-and-facts.md
   3 have no IR equivalent (ansible.builtin.assemble) — would need `exec`
```

It never emits IR. It answers "what would this cost" without pretending to do it, and it
is honest by construction because reporting a gap is the whole output rather than an
error case.

---

## 2. Coexistence model

Forward-only compilation is only viable if a Meridian-managed `ResourceSet` and a
hand-written playbook can manage the same node without fighting. They can, within a
boundary that the tool enforces rather than documents.

**Decision: coexistence at resource granularity, with declared ownership and detected
conflict.**

### 2.1 The ownership boundary

Meridian owns the resources it declares. It does not own the node. Concretely: Meridian
never emits anything that removes, reverts, or asserts absence of a resource it was not
asked to manage. There is no "purge unmanaged resources" mode, and there will not be one,
because it is the feature that makes incremental adoption impossible.

This differs from Puppet's `resources { purge => true }` posture on purpose. Puppet
assumes it owns the node. Meridian assumes, during adoption, that it does not.

### 2.2 Conflict detection

The real risk is not coexistence but overlap: Meridian manages `nginx.conf` and so does a
legacy playbook, and the two fight on alternating runs. This is the failure that makes
teams abandon a migration, and it is silent for weeks before it is noticed.

Meridian declares an **ownership manifest** alongside each emitted artifact, listing every
concrete system object the artifact touches, derived from the resource parameters:

```yaml
# .meridian/ownership/web-server-baseline.yaml
owned:
  - kind: package
    name: nginx
  - kind: file
    path: /etc/nginx/nginx.conf
  - kind: service
    name: nginx
```

`meridian plan` reads every ownership manifest under the run root and hard-fails on
overlap between two `ResourceSet` documents. That is a contradiction within Meridian and
there is no reason to tolerate it.

Overlap with configuration Meridian does not manage cannot be detected this way, because
Meridian has no parser for the other tool (§1). Instead, `meridian assess` (§1.3) reports
it, and the operations documentation states plainly that during a migration this remains
the user's responsibility. Saying so is better than implying a protection that does not
exist.

### 2.3 Per-tool coexistence notes

| Target | Coexists with hand-written config | Caveat |
|---|---|---|
| Ansible | Yes, cleanly | Separate playbook files; no shared handler namespace, so a Meridian handler cannot be triggered by a legacy task |
| Puppet | Yes, with care | Global resource-title uniqueness is catalog-wide. Two `package { 'nginx': }` declarations are a compile error in the *agent*, not in Meridian. Documented in `targets/puppet.md` |
| Chef | Yes | Separate cookbook; run-list ordering is the user's to sequence |
| DSC classic | **No, not safely** | The LCM applies one configuration document per node. A Meridian MOF replaces the existing one wholesale. Partial configurations are the only supported path and must be set up first |
| DSC v3 | Yes | No LCM, so no single-document constraint |
| Terraform | Yes, with separate state | Never share a state file. A separate root module and backend key. Cross-reference by remote state data source, not by importing into Meridian's run |

The DSC-classic row is the one that will surprise people, and it is a property of the LCM
rather than of Meridian. It gets a prominent callout in `targets/dsc-classic.md`, not a
footnote, because a user who discovers it by having their existing configuration replaced
has had a very bad day.

---

## 3. `exec` as a deliberate on-ramp

Design plan §4.3 includes `exec` as a raw escape hatch. For migration it has a second,
explicitly sanctioned role: wrapping an existing imperative script so a team can move a
workflow under Meridian's orchestration before it has been modelled as resources.

**Decision: `exec` as a migration bridge is supported and documented, and is visible in
plan output so it does not become permanent by accident.**

The pattern:

```yaml
- id: legacy_app_deploy
  type: exec
  params:
    command: /opt/legacy/deploy.sh
    creates: /opt/app/current/.deployed   # idempotency guard, see below
  dependsOn:
    - resource: app_pkg
```

Three rules make this a bridge rather than a trapdoor:

1. **`exec` is never idempotent by default, and Meridian says so.** Design plan §9 notes
   that idempotency is provider-enforced on Puppet, DSC and Terraform, and
   convention-only on Ansible and Chef. `exec` has no provider. An `exec` without a
   `creates`, `unless`, or `onlyIf` guard produces a plan-time warning on every target,
   including the ones where the native escape hatch would not have warned.
2. **`exec` count is reported.** `meridian plan` prints the ratio, so migration progress
   is measurable and regression is visible:
   ```
   resources: 41 total — 34 typed, 7 exec (17%)
   ```
   The number is not a failure. It is only useful if nobody is ashamed of it, so the
   documentation frames a high starting ratio as expected.
3. **`exec` is target-specific in ways the IR cannot hide.** Shell escaping, working
   directory defaults and environment propagation differ across `shell`, `exec`,
   `Script` and `provisioner`. An IR document that is heavy on `exec` is correspondingly
   less portable, and `targets/*.md` documents each target's rules rather than implying a
   uniform one.

The honest framing for documentation: `exec` lets you adopt Meridian's orchestration and
data resolution now, and convert to typed resources later, one resource at a time,
with the ratio showing progress. It does not give you portability for the wrapped script.

---

## 4. Pilot adoption guidance

Design plan §1 observes that package and service management is the easy 80%. That has
never been turned into advice for a team choosing where to start. It is now.

**Decision: the recommended pilot is a single-target, single-role `ResourceSet` covering
package, file and service, on the target the team already runs in production.**

### 4.1 Start with the target you already have

The instinct is to pilot on a target the team is migrating *to*. That is the wrong order.
Piloting on the tool you already run means the output is reviewable by people who can
read it, the failure modes are familiar, and the comparison is direct: the emitted
playbook either does what the hand-written one did, or it does not. Nothing about
Meridian's value depends on multi-target output being exercised on day one, and
introducing a new target and a new tool at once means a failure cannot be attributed to
either.

### 4.2 The recommended sequence

| Step | What | Why this order |
|---|---|---|
| 1 | `meridian assess` on one existing role | Finds the gaps before any commitment. Costs an afternoon |
| 2 | Author one `ResourceSet`: package, file, service, one `dependsOn`, one `notifies` | The "easy 80%" path. Exercises the parts that map cleanly on every target |
| 3 | Compile and **diff against the existing artifact**. Do not apply | Review is where trust is built. The static-output guarantee exists to make this diff readable |
| 4 | Apply in a non-production environment; apply twice, confirm the second run is a no-op | Idempotency is the property most likely to differ from the hand-written original |
| 5 | Move hierarchy data into `hierarchy.yaml` | Deliberately after step 4: changing the data layer and the emission layer at once makes a diff impossible to attribute |
| 6 | Add a second target, compile the same IR, read the output | The first real payoff, and safe because the IR is already proven on one target |
| 7 | Only now, the two-stage `Infrastructure` → `ResourceSet` handoff | The most powerful and most complex feature. It needs the single-stage model to be understood first |

### 4.3 What not to pilot on

Stated explicitly, because these are attractive and wrong:

- **Not the most complex role.** Complex roles are `exec`-heavy and `runtimeWhen`-heavy,
  and evaluate the escape hatches rather than the abstraction.
- **Not DSC classic**, unless the team is already committed to DSC. It carries the
  synthetic-notify workaround (design plan §8), the LCM coexistence constraint (§2.3),
  and the heaviest CI requirements (testing plan §10).
- **Not DSC v3 as the very first target.** It is the newest plugin and the testing plan
  (§7) flags it as higher-risk until it has a track record. A pilot should not be the
  thing that gives it one.
- **Not a two-stage `Infrastructure` → `ResourceSet` flow first.** It is the best
  demonstration and the worst starting point, because a failure could be in provisioning,
  fact gathering, the handoff, or configuration, and a team with no baseline cannot tell
  which.

### 4.4 Effect on quickstart documentation

Per documentation plan §4, this decision gates the quickstart content and should be
settled before it is written rather than after.

- `docs/quickstart/first-resourceset.md` follows steps 2–4, on Ansible.
- `docs/quickstart/first-two-stage.md` is explicitly marked as step 7 and links back, so a
  reader who arrives there first is told to start elsewhere.
- A new `docs/operations/migration.md` carries §2 and §3 of this document: coexistence,
  the ownership manifest, per-target caveats, and the `exec` bridge pattern.

Documentation plan §4's rationale applies here too: none of this should be written as
aspiration. `meridian assess` is documented when it exists, not before.
