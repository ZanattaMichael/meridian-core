# Security & Secrets Handling Model

> Status: **accepted**. Resolves issue #8.
> Amend by pull request against this file.

Meridian moves secrets across a handoff point (`Infrastructure` output →
`ResourceSet` input) and routes them to five structurally different per-target secrets
subsystems: Ansible Vault, Puppet eyaml, Chef encrypted data bags, Terraform
`sensitive`, and DSC certificate-based MOF encryption. There is no shared
implementation underneath those five, so there cannot be one shared guarantee on top of
them. This document defines which guarantee holds where.

---

## 1. Threat model

### 1.1 In scope

Meridian is responsible for secrets while they are inside its own pipeline.

| Asset | Exposure Meridian owns |
|---|---|
| Secret values in transit through the pipeline | `resolve` → `condition` → `transform` → `emit` all hold plaintext in memory; any of these stages can leak to logs, plan output, or error messages |
| The `Infrastructure` → `ResourceSet` handoff | A new channel that exists only because Meridian exists. No target tool has an equivalent, so no target tool's own protections apply |
| Meridian's run-state store (design plan §11.5) | A file Meridian creates and writes. Nothing else in the stack governs its contents |
| Emitted artifacts | Meridian decides whether a secret is baked in as a literal or routed to the target's secrets subsystem |
| Plugin-to-plugin handoff over the gRPC plugin boundary | Secret values cross a process boundary as protocol messages |

### 1.2 Out of scope

Meridian does not own, and must not claim to improve, the at-rest security of each
target tool.

- Ansible Vault's key management, Puppet eyaml's key distribution, Chef's data-bag
  secret file, the certificate lifecycle behind MOF encryption. Meridian hands a value
  to the mechanism; operating the mechanism is the user's job.
- **Terraform state is the sharpest case.** A `sensitive` value in Terraform is masked in
  CLI output and stored in plaintext in state. Meridian cannot fix this, and pretending
  otherwise would be the single most misleading claim the project could make. See §5.
- Transport security of the target tool's own execution (SSH host key policy, Puppet
  agent certificate validation).
- The security of user-authored `exec` escape-hatch content. Meridian escapes it
  correctly for the target shell; it does not audit what it does.

### 1.3 Adversaries assumed

Someone who can read CI logs, someone who can read the repository containing emitted
artifacts, and someone who can read Meridian's run-state file. Not assumed: an
adversary with live memory access to the running Meridian process. Protecting plaintext
in process memory from a local root adversary is not a goal, and claiming it would
require guarantees Go's runtime does not offer.

---

## 2. What `secret: true` guarantees, per stage

Masking in output and encryption at rest are different guarantees. Conflating them is
how a user ends up trusting a plaintext file. The two are tracked separately below.

| Stage | Guarantee | Mechanism |
|---|---|---|
| `resolve` | Value is loaded and carried as a `secrets.Value`, never a bare `string` | The type has no exported accessor returning a plain string other than `Reveal()`, which is called in exactly one place per plugin |
| `resolve` | `String()`, `Format()`, `MarshalJSON()` and `MarshalYAML()` on `secrets.Value` return `***` | Makes the safe behavior the default for every logger, formatter and serializer, including ones not written yet |
| `condition` | A `when` expression **may** read a secret, and the result is a boolean, not the value | Branching on a secret is legitimate. Emitting the secret into the pruning trace is not: the trace records the resource id and the outcome, never the operand |
| `gather` | Gathered facts are **never** marked secret automatically | A fact is host state, not a credential. A user who needs a secret fact declares it; silently promoting facts to secret would hide real values behind a mask and make debugging impossible |
| `graph` / `sort` | No access to values at all | These stages operate on ids and edges |
| `emit` | Secret values are **excluded by construction** from the literal-baking path | See §3 |
| `plan` output | Masked | A `secrets.Value` cannot render itself in plaintext |
| Error messages | Masked | Errors carry the resource id and the parameter name, never the value. Enforced by the same type |

**The guarantee Meridian does not make:** that a secret is encrypted at rest once it
leaves Meridian. That is the target subsystem's job and it varies per target. §5 states
the per-target position.

---

## 3. The literal-baking path

Design plan §5 establishes that resolved values are baked into emitted artifacts as
literals by default, because self-contained, diffable output is worth a lot. That
default is **wrong for secrets**, and being wrong by convention is not acceptable.

**Decision: exclusion is structural, not conventional.**

The emitter is given resolved data in which secret values are already replaced by
opaque references. The literal-baking code path receives `secrets.Ref{ID: "db_password"}`
where a plaintext string would otherwise be. There is no code path from that type to a
plaintext literal in an artifact, because the type does not carry the plaintext at all.
A plugin that wants the real value must resolve the reference through the target's
secrets subsystem, which is a different, explicit call.

This means a plugin author cannot accidentally do the wrong thing by forgetting a check.
The wrong thing is not expressible.

Enforcement, layered:

1. **Type system.** `secrets.Ref` has no plaintext accessor.
2. **Contract test.** Testing plan §5.1 `fixture_secret_value.yaml` asserts the fixture's
   secret string appears in no emitted artifact for any target. Failures are high
   severity.
3. **Scan.** Testing plan §8 runs a pattern scan over every emitted artifact across all
   fixtures. This catches a plugin that reconstructs a secret by an unanticipated route,
   such as string-concatenating it into an `exec` command body.

Item 3 exists because items 1 and 2 both assume the plugin author is working within the
intended shape. The scan makes no such assumption.

---

## 4. The run-state store

Design plan §11.5 gives Meridian its own run-state:
`infra_id → resourceset_id → last_applied_hash → status`.

**Decision: no secret value is ever persisted to run-state, in any form, including
hashed.**

Note that the schema above already contains no value fields. That is the point — it was
designed to hold identifiers and status, not data. This decision records it as a
constraint rather than an accident, so a later change that adds a value field is visibly
a policy change.

Specifically:

- `last_applied_hash` is a hash of the **emitted artifact**, and artifacts never contain
  plaintext secrets (§3). It is not a hash of resolved input data.
- A hashed secret is still a secret for this purpose. A low-entropy secret is
  brute-forceable from its hash, and "we only stored the hash" is the kind of reassurance
  that turns out to be worthless on exactly the values people most want protected.
- Failure diagnostics written to run-state carry resource ids, not parameter values.

Run-state is written with mode `0600`. That is defence in depth, not the primary control:
the primary control is that there is nothing sensitive in the file.

---

## 5. Terraform's plaintext-in-state problem

Terraform's `sensitive = true` masks a value in CLI output and writes it to state in
plaintext. A user who reads "Meridian supports secrets on Terraform" and infers
encryption at rest has been misled, and Meridian will have been the thing that misled
them.

**Decision: warn loudly at plan time, do not attempt to mitigate.**

When a value marked `secret: true` flows into a Terraform resource or output,
`meridian plan` emits a capability degradation notice in the same inline format as every
other degradation (see [observability](observability.md#3-plan-output)):

```
target: terraform — secret 'db_password' will be stored in PLAINTEXT in Terraform state.
  Terraform's `sensitive` masks CLI output only; it does not encrypt state.
  Mitigation is your state backend's encryption, which Meridian does not control.
  docs: /operations/secrets-handoff#terraform-state
```

Three mitigations were considered and rejected:

- **Refuse to compile.** Too strict. Plaintext in a properly-configured encrypted remote
  backend is a normal, accepted posture for many teams. Meridian is not positioned to
  overrule that.
- **Encrypt the value ourselves before it reaches Terraform.** Terraform cannot decrypt
  it, so the resource receives ciphertext where it needs a credential. This does not work.
- **Silently route through a Vault provider data source.** Rewriting a user's
  infrastructure to add a dependency on a secrets manager they did not ask for is a
  larger and more surprising action than the problem justifies.

Warning is the honest option. It is also the one consistent with the design plan's
no-silent-degradation principle: the degradation is real, so it gets surfaced.

**Escalation path:** `meridian plan --strict-secrets` promotes this warning to a hard
error, so a team that has decided plaintext state is unacceptable can enforce that in CI
rather than by review.

---

## 6. Plugin SDK secrets contract

A plugin claiming secrets support must satisfy all of the following. This is mirrored in
the `Capabilities{}` struct so the claim is machine-checkable, per design plan §11.4.

```go
type Capabilities struct {
    NativeNotify     bool
    SyntheticNotify  bool
    RuntimeCondition bool

    // SecretsBackend names the target's native secrets subsystem, or is empty
    // if the target has none. A non-empty value obliges the plugin to the
    // contract below and is verified by the shared secrets contract suite.
    SecretsBackend   string // "ansible-vault" | "eyaml" | "chef-databag" |
                            // "terraform-sensitive" | "dsc-mof-cert" | ""
    SecretsAtRest    bool   // true only if the backend encrypts at rest.
                            // Terraform sets this false. This field is why
                            // masking and encryption cannot be conflated.
}
```

Obligations of a plugin with a non-empty `SecretsBackend`:

1. **Never call `Reveal()` outside its secrets-routing function.** One call site, greppable,
   reviewed. A plugin with `Reveal()` scattered through `transform.go` fails review.
2. **Emit secrets only into the named backend.** Never into a task parameter, a manifest
   literal, an `exec` argument, or a generated comment.
3. **Pass the shared secrets contract suite**, which runs `fixture_secret_value.yaml`
   against the plugin and asserts the plaintext appears in no emitted file.
4. **Declare `SecretsAtRest` truthfully.** A false claim here is a security bug, not a
   documentation bug, and is handled under §7 rather than through normal issue triage.
5. **Surface the degradation** when `SecretsAtRest` is false, in plan output, as in §5.

A plugin with an empty `SecretsBackend` must **hard-fail** compilation when any
`secret: true` value reaches it. Not warn, not bake it in. This is the design plan's
no-silent-capability-loss rule applied to the case where silence is most costly.

Per [governance](governance.md#4-security-review-requirement), every plugin touching
this contract requires a second reviewer regardless of plugin tier.

---

## 7. Vulnerability disclosure

Meridian compiles artifacts that configure production infrastructure and handles
credentials in transit. It needs a real disclosure process before it has real users, not
after.

**Reporting.** Private vulnerability reporting via GitHub Security Advisories on
`meridian-core`, which keeps the report, the fix, and the CVE in one place and does not
require the project to operate a mailbox. Advertised in `SECURITY.md` at the repository
root, where GitHub surfaces it automatically.

**Commitments.**

| Stage | Target |
|---|---|
| Acknowledgement | 3 working days |
| Initial assessment and severity | 10 working days |
| Fix or documented mitigation, critical and high | 30 days |
| Fix or documented mitigation, medium and low | Next scheduled release |
| Public advisory | On fix release, or at 90 days, whichever is first |

The 90-day backstop is stated so a reporter knows the issue becomes public whether or
not the project has managed to fix it. A project that reserves the right to sit on a
report indefinitely does not get good reports.

**Scope for reporters.** In scope: anything in §1.1. Explicitly in scope and worth
calling out, because they are the failure modes this design most fears:

- A secret reaching an emitted artifact in plaintext.
- A secret reaching logs, plan output, or run-state.
- A path that defeats the §3 structural exclusion.
- An `exec` escaping bug allowing injection into a generated artifact.

Out of scope: §1.2 items, and reports that a target tool's own secrets mechanism is
weak. Those belong upstream.

**Safe harbour.** Good-faith research following this policy will not be pursued. Testing
must be against the reporter's own infrastructure.

**Credit.** Reporters are credited in the advisory unless they decline.
