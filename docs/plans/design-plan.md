# Meridian — Design Plan

> Status: **accepted**. Originally authored in issue #1; this file is the canonical copy.
> Changes to this plan are made by pull request against this file, not by editing the issue.

**Repository:** `meridian-core`
**One-line description:** A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend.

---

## 1. Problem Statement

Configuration management (Ansible, Puppet, Chef, DSC) and infrastructure-as-code (Terraform, ARM) tools each expose their own DSL, execution model, and state semantics. Teams working across multiple backends re-implement the same intent five different ways. Meridian aims to provide a **single declarative DSL** (YAML/JSON) that developers author once, which Meridian then compiles into native artifacts for each target tool.

The core design challenge is **not syntax** — YAML/JSON as a surface format is trivial. The challenge is that these tools differ in fundamental ways:

- **Execution model**: push (Ansible) vs. pull (Puppet/DSC agent) vs. plan/apply (Terraform) vs. compiled imperative (Chef).
- **State tracking**: Terraform owns external state; Ansible/Chef have none by default; Puppet/DSC compile a catalog per run.
- **Dependency expression**: implicit ordering (Ansible, Chef) vs. explicit graph metaparameters (Puppet, DSC) vs. implicit-via-reference (Terraform).
- **Idempotency guarantee**: enforced by the resource/provider layer (Puppet, DSC, Terraform) vs. convention-only (Ansible, Chef).

Meridian's job is to **abstract intent**, not to pretend these differences don't exist. Where they can't be reconciled, Meridian must surface the gap explicitly rather than silently degrading behavior.

---

## 2. Guiding Principles

1. **Compile, don't unify runtimes.** Meridian is a transpiler + orchestrator, not a reimplementation of five execution engines. Each backend keeps doing what it's good at (Terraform's graph/state, Ansible's SSH orchestration, etc.).
2. **Static output wherever possible.** All conditional logic (`when`) is evaluated by Meridian itself, ahead of compilation, against resolved hierarchy data and gathered node facts. Generated artifacts (playbooks, manifests, MOF, HCL) contain **no embedded conditionals** — they are fully static, auditable, and diffable.
3. **No silent capability loss.** Every target plugin declares what it supports (`Capabilities{}`). When a feature (e.g. `notifies`) has no clean native mapping on a target, Meridian either emits a documented workaround or hard-fails at compile time — never a silent downgrade.
4. **Two-tier resource model.** Provisioning (VMs, networks — Terraform/ARM) and host configuration (packages, services — Ansible/Puppet/Chef/DSC) are treated as structurally distinct `kind`s in the IR, not one universal resource graph.
5. **Plugin-based targets.** Every backend, including Meridian's own first-party support, is implemented against the same public SDK interface a third party would use. No special-cased core code.
6. **Deterministic compilation.** Given the same IR and the same resolved data, output must be byte-identical across runs. No map-iteration-order or hash-order nondeterminism.

---

## 3. Framework Name

**Meridian** — chosen to evoke "a single reference line that many different systems align to," matching the compile-to-many-targets architecture rather than a universal executor.

Runner-up names considered: Isogloss, Polyform, Traverse, Cascade.

---

## 4. IR Schema Overview

Two top-level `kind`s, linked via output references:

### 4.1 `kind: Infrastructure` (provisioning layer → Terraform / ARM)

```yaml
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web-vm
spec:
  target: terraform
  resources:
    - id: web_vm
      type: compute_instance
      params:
        image: ubuntu-22.04
        size: small
        region: us-east-1
  outputs:
    - name: web_vm_ip
      value: "${web_vm.public_ip}"
```

### 4.2 `kind: ResourceSet` (host configuration layer → Ansible / Puppet / Chef / DSC)

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web-server-baseline
spec:
  target: ansible
  targetHost:
    from: infrastructure
    ref: web-vm
    output: web_vm_ip
  resources:
    - id: nginx_pkg
      type: package
      state: present
      params:
        name: nginx

    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx/nginx.conf
        source: templates/nginx.conf.j2
      dependsOn:
        - resource: nginx_pkg
          ifMissing: error
      notifies:
        - resource: nginx_svc
          on: changed
          action: restart

    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
      dependsOn:
        - resource: nginx_pkg
```

### 4.3 Key IR Fields

| Field | Purpose | Evaluated by |
|---|---|---|
| `dependsOn` | Hard ordering constraint ("must happen before") | Meridian graph/sort stage; compiled to each target's native ordering mechanism |
| `notifies` | Conditional, deferred trigger ("run X only if Y changed") | Target-specific; native on Ansible/Puppet/Chef, synthetic on DSC/Terraform |
| `when` | Conditional inclusion of a resource | Meridian, at compile time, against resolved hierarchy data + gathered facts — **never** emitted into target output |
| `runtimeWhen` | Rare escape hatch: condition evaluated at the instant of apply, on the target itself | Target-native, only where `Capabilities.RuntimeCondition == true` |
| `secret` | Marks a value as sensitive | Routed to per-target secrets subsystem (Vault/eyaml/data bags/`sensitive`/certs) — no shared implementation |
| `exec` (resource type) | Raw escape hatch for anything without a clean mapping | Per-target shell/script emission with target-specific escaping rules |

---

## 5. Data Resolution Layer (Hiera-style, not Datum-dependent)

A hierarchical, layered config resolution engine, implemented natively in Meridian's core language (not PowerShell/Datum), inspired by Puppet's Hiera:

```yaml
# hierarchy.yaml
hierarchy:
  - common.yaml
  - "environment/%{env}.yaml"
  - "role/%{role}.yaml"
  - "node/%{node}.yaml"
```

- Per-key merge strategies: `first-found` (highest-priority layer wins), `deep-merge` (recursive hash merge), `unique-array-merge` (concatenate + dedupe).
- Pure function: `resolve(node, hierarchy) → flat_data_map`. Deterministic, cacheable, unit-testable.
- **Default behavior**: resolved values are baked into emitted artifacts as literals (self-contained, diffable output).
- **Opt-in mode**: "native passthrough" emits *into* each tool's own hierarchy mechanism (`group_vars`, Hiera `common.yaml`, `ConfigurationData`) for teams with existing investment — not the default, to avoid reintroducing per-tool divergence.

Why not use Datum (PowerShell) directly: introduces a runtime dependency most target users (Linux Ansible/Puppet control nodes) don't already have, couples the framework's identity to the DSC ecosystem, and the merge logic itself is straightforward enough to own natively and keep deterministic/embeddable.

---

## 6. Condition & Fact-Gathering Pipeline

Per the decision that all conditions should be evaluated by Meridian itself, not emitted into target output:

```
IR → resolve (hierarchy merge) → gather (live node facts) → condition (evaluate + prune)
   → graph (build + topological sort) → transform → validate → emit
```

### 6.1 Fact gathering

For `ResourceSet` runs against already-existing nodes, Meridian connects to the node **before compilation** and gathers facts (file existence, installed versions, OS release, etc.), merged into resolved data under a `facts.*` namespace, distinct from hierarchy data for provenance/debugging.

```yaml
when: "facts.file_exists('/etc/old-app.conf')"
```

### 6.2 Orchestration ordering for freshly-provisioned nodes

Facts can't be gathered from a node that doesn't exist yet. The orchestrator sequences:

```
Infrastructure.Apply() → node reachable
    → orchestrator.GatherFacts(node)
    → ResourceSet.Resolve + Condition + Compile (fact-aware)
    → ResourceSet.Emit (fully static artifact)
    → ResourceSet.Apply
```

### 6.3 Known trade-off: staleness

Gather-then-prune trades just-in-time accuracy (which target-native runtime conditionals like Ansible's `when:` always have) for static, auditable output. Mitigation: `meridian apply` warns or requires `--refresh-facts` if a plan is older than a configurable threshold, similar to Terraform's stale-plan warnings.

### 6.4 Dangling edges after pruning

If a `when`-pruned resource is the target of a `dependsOn` or `notifies` edge, policy is explicit per edge:

```yaml
dependsOn:
  - resource: nginx_pkg
    ifMissing: error   # or: skip
```

Default: `error` (safer — surfaces likely authoring mistakes at compile time).

### 6.5 Remaining runtime-only escape hatch

`runtimeWhen` is retained for the narrow case where a condition must be evaluated at the literal instant of apply (e.g., checking immediately before a restart whether another process is mid-write). Only valid where `Capabilities.RuntimeCondition == true` (not DSC classic — MOF cannot branch at apply time).

---

## 7. Dependency Graph & Target-Specific Sort

`dependsOn` compiles differently per target because "dependency" is not one concept:

| Target | Compiled form |
|---|---|
| Ansible | No native graph — flattened into a strict linear task order via deterministic topological sort (stable tiebreak by declaration order in source IR, not map/hash order) |
| Puppet | `require =>` / `before =>` metaparameters — graph structure preserved directly |
| DSC | `DependsOn = '[Type]Name'` array — graph structure preserved directly |
| Terraform | Implicit via attribute reference where possible, explicit `depends_on` otherwise |
| Chef | Recipe order (flattening, same problem as Ansible) + `notifies`/`subscribes` for event-driven ordering |

Cycle detection is a **hard compile error** across all targets. Ansible's flattening step is the earliest and cleanest place this surfaces, since it has no native way to even express a contradictory graph.

**Pipeline ordering rule:** transform before sort. Some sort decisions (e.g. Terraform's implicit-vs-explicit dependency choice) depend on the native resource shape, which only exists after transform.

---

## 8. The `notifies` Property — Capability Matrix

`notifies` is distinct from `dependsOn`: it is a **conditional, deferred trigger** ("run X only if Y changed"), not an ordering constraint. Represented in the graph as a separate edge kind:

```go
type EdgeKind int
const (
    HardOrder EdgeKind = iota  // dependsOn
    Notify                     // notifies — conditional, deferred
)
```

| Target | Support | Mechanism |
|---|---|---|
| Ansible | Native | `notify:` + `handlers:` block, deduped, deferred to end of play |
| Puppet | Native | `~>` (subscribe) refresh-on-change |
| Chef | Native | `notifies :restart, ..., :delayed` / `:immediately` |
| DSC (classic, MOF/LCM) | Synthetic | `Script` resource wrapping target, with hash-file-based `TestScript` change-detection — adds local state file, real ongoing complexity |
| DSC v3 | TBD | Needs research — JSON-native, no MOF; may have a real change-detection primitive, don't assume the classic workaround carries over |
| Terraform | Synthetic (tiered) | Tier 1: native provider-level change-trigger if one exists; Tier 2: `terraform_data` + `triggers_replace`, which forces resource replacement — shows as destroy/create in plan, least idiomatic of all targets |

Declared per plugin via SDK:

```go
type Capabilities struct {
    NativeNotify     bool
    SyntheticNotify  bool
    RuntimeCondition bool
}
```

`meridian plan` surfaces this per target explicitly, e.g.:

```
target: dsc       — notify supported via synthetic state tracking (adds local state file)
target: terraform — notify supported via forced resource replacement (shows as destroy/create in plan)
target: ansible   — notify supported natively
```

---

## 9. Other IR Properties Likely to Leak Across Targets

| Property | Divergence |
|---|---|
| Conditionals | Runtime (Ansible/Chef) vs. compile-time (DSC MOF) vs. plan-time (Terraform) — resolved by Meridian's gather-and-prune model (Section 6), removing this as a per-target concern |
| Idempotency guarantee | Enforced by provider (Puppet/DSC/Terraform) vs. convention-only (Ansible/Chef) — IR cannot assume declared state implies guaranteed idempotent execution |
| Secrets | Five distinct mechanisms (Ansible Vault, Puppet eyaml, Chef encrypted data bags, Terraform `sensitive` — masked in output only, not encrypted at rest in state — DSC certificate-based MOF encryption). No shared implementation; route per target |
| Retry/timeout | Native in Ansible (`retries`/`until`/`delay`) and Terraform provisioners (`timeout`); absent in Puppet outside `exec`; partial in Chef |
| Privilege escalation | Ansible `become`, Chef `sudo` pattern; no per-resource concept in Puppet/DSC (agent assumed privileged) or Terraform provisioners |
| Raw exec / escape hatch | Every target has one (`shell`/`command`, `exec`, `Script`, `provisioner`) — each with different shell-escaping, cwd defaults, and env-var passing rules |
| Tags/selective apply | Ansible tags; no native equivalent in Puppet (node classification instead) or Terraform (`-target` discouraged) |

Same governing rule as `notifies`: a shared IR field does not imply shared runtime behavior. Every field needs a per-target capability declaration.

---

## 10. DSC Versioning — A Structural Split, Not a Version Pin

DSC is not one target with version drift — it is at least **two structurally different tools** sharing a vendor name:

- **DSC classic (v1/v2)**: MOF-compiled, Local Configuration Manager (LCM)-driven, function- or class-based PowerShell resources.
- **DSC v3**: Rust-rewritten `dsc` CLI. No LCM. No MOF. Resources are standalone executables/scripts speaking JSON over stdin/stdout against a defined manifest schema. Config documents are native YAML/JSON, not compiled PowerShell `Configuration` blocks.

**Design decision:** DSC version is a first-class target dimension, routing to entirely separate plugins — not a flag inside one plugin.

```yaml
spec:
  target: dsc
  targetVersion: "3.0"     # routes to a distinct plugin
```

```
plugins/
├── dsc-classic/     # v1/v2 — MOF compilation, LCM
└── dsc-v3/          # v3 — JSON/YAML native, no MOF
```

Each has its own `Capabilities{}` — do not assume DSC v3 inherits DSC classic's limitations or workarounds (e.g. the hash-file `notifies` synthesis) without verifying against v3's actual resource manifest spec.

**Broader implication:** `target` should resolve to a `(tool, version)` pair that can route to a different plugin implementation, not just different emitted syntax. Chef's Cookbook API and Terraform's 0.11→0.12 HCL2 rewrite are flagged as other candidates for this same treatment.

---

## 11. System Architecture (Go)

### 11.1 Language choice: Go

Chosen over Rust, Python, TypeScript, and Ruby for:
- Single static binary, no runtime dependency for end users (who already install 3-5 native CaC tools).
- Mature YAML/JSON ergonomics for heavy structured-data manipulation.
- Precedent alignment: Terraform, Pulumi, and Puppet's Bolt/PDK tooling all use Go for the same class of problem.
- Native gRPC support, easing a Terraform-style plugin protocol.
- Goroutines/channels fit the orchestrator's need to run independent stages concurrently.
- Faster iteration than Rust for a core whose resource-mapping tables will churn heavily; more distributable than Python/Node for a CLI product.

### 11.2 Project layout

```
meridian/
├── cmd/
│   └── meridian/              # CLI entrypoint (plan, apply, resolve, validate)
├── internal/
│   ├── ir/                    # IR types + YAML/JSON parsing, schema validation
│   ├── resolve/                # hierarchical data resolution (Hiera-style)
│   ├── facts/                  # live node fact gathering
│   ├── condition/               # when-evaluation + AST pruning
│   ├── graph/                  # dependency graph, topological sort, cycle detection
│   ├── ast/                    # internal target-agnostic resource tree
│   ├── plugin/                 # plugin protocol (gRPC), host-side loader
│   ├── orchestrator/           # stage sequencing, run-state tracking
│   └── secrets/                # masking/handling values flowing infra -> resourceset
├── plugins/                    # built-in emitters, each its own compiled plugin
│   ├── ansible/
│   ├── puppet/
│   ├── terraform/
│   ├── dsc-classic/
│   ├── dsc-v3/
│   └── chef/
├── pkg/
│   └── sdk/                    # public plugin SDK (third parties build against this)
│       └── pluginpb/           # generated protocol code, committed so protoc is not a build dependency
├── proto/                      # the .proto definition of the plugin protocol
├── schema/                     # JSON Schema for the DSL — validation + editor tooling
├── go.mod
└── go.sum
```

### 11.3 Per-plugin internal stages

```
plugins/<target>/
├── transform.go     # IR resource type -> native resource type + param mapping; can reject unsupported types
├── sort.go          # target-specific dependency/ordering semantics
├── emit.go          # pure serialization: native AST -> target syntax (no mapping/ordering logic)
├── validate.go       # target-specific constraint checks (naming, uniqueness, etc.)
├── runner.go         # invokes the real CLI, captures outputs — kept outside the compile chain
└── cmd/
    └── meridian-target-<target>/   # the plugin binary: wiring only, no target logic
```

The binary is a separate package from the target so the emitter can be tested without a
process boundary and the boundary can be tested without a target. A plugin binary is named
`meridian-target-<target>`, which is how a host discovers one without executing it first.

Sequencing rule: **transform → sort → validate → emit**, chained inside the plugin's `Emit()`. `Runner` is deliberately separate from `Emitter` so `meridian plan` remains a pure, side-effect-free operation.

### 11.4 Core SDK interfaces

```go
type Emitter interface {
    Name() string
    SupportedResourceTypes() []string
    Capabilities() Capabilities
    Emit(*ResourceGraph, Data) (Artifact, []Warning, error)
}

type Capabilities struct {
    NativeNotify     bool
    SyntheticNotify  bool
    RuntimeCondition bool
}

type Artifact struct {
    Files map[string]string  // relative path -> file content
}

type Warning struct {
    Target   string
    Resource string
    Msg      string
}

type Runner interface {
    Name() string
    Apply(ctx context.Context, artifact Artifact) (Outputs, error)
    Plan(ctx context.Context, artifact Artifact) (Diff, error)
}
```

Two refinements the implementation made to this sketch, both load-bearing:

- The tree a target receives is `sdk.ResourceGraph`, not the compiler's `ast.ResourceGraph`.
  The internal tree owns invariants it built and publishing it would make every later change
  to the compiler's representation a breaking change for third-party plugins.
- `Emit` returns warnings alongside the artifact rather than folding them into the error.
  A warning is a successful compile that lost something (Ansible flattening parallelism, say),
  and collapsing it into the error channel would force a target to choose between reporting
  the loss and producing output.

### 11.5 Orchestrator responsibilities

1. Apply `Infrastructure` docs in dependency order via native CLIs (`terraform apply`, ARM deployment).
2. Read back outputs from each tool's own state.
3. Gather facts from newly-reachable nodes.
4. Resolve + condition-prune + compile `ResourceSet` docs (fact-aware).
5. Apply `ResourceSet` docs via native CLIs.
6. Track its own lightweight run-state (`infra_id → resourceset_id → last_applied_hash → status`) — necessary because Terraform's state doesn't capture ResourceSet-stage success, and Ansible/Chef track no state of their own.

### 11.6 Known architectural leaks (documented, not hidden)

- **State ownership conflict**: Terraform's state and Meridian's own run-state are two separate sources of truth that must be reconciled, not merged.
- **Drift asymmetry**: Terraform detects drift on `plan`; Ansible/Chef don't track drift at all between runs; Puppet/DSC agents poll and self-correct on an interval. Documented per-target, not papered over in the schema.
- **Secrets handoff**: any value that flows `Infrastructure` output → `ResourceSet` input needs the same masking discipline as Terraform's own sensitive outputs, since the handoff channel is a new potential leak point.

---

## 12. Build Sequencing (Milestones)

1. **`ir` + `resolve` + `graph`** — parse, hierarchical merge, topological sort. Fully unit-tested, no emitters at all.
2. **One emitter, no plugin boundary** — hardcode Ansible directly in `internal/` to validate the AST shape end-to-end before paying the cost of a gRPC plugin boundary.
3. **Extract that emitter across `pkg/sdk`** as the first real plugin — proves the plugin-loading mechanism against an already-correct emitter.
4. **Add Puppet and Terraform emitters** — the real test of whether the AST is target-agnostic; Terraform's provisioning model and Puppet's catalog model will expose any hidden Ansible-shaped assumptions from step 2.
5. **Add DSC-classic, DSC-v3, Chef** — exercises the full capability-matrix machinery (synthetic notify, runtime-condition gating, per-target validation).

Repo/org structure should stay single-repo (`meridian` or `meridian-core`) until 2-3 plugins have been built and the SDK boundary has proven stable — splitting into a multi-repo plugin org (mirroring Terraform's `terraform-provider-*` pattern) is a later step, not a day-one decision.

---

## 13. Open Questions

- Does DSC v3's native resource manifest expose a real change-detection primitive, avoiding the need for DSC classic's hash-file `notifies` workaround?
- What staleness threshold should trigger a `--refresh-facts` requirement on `meridian apply`?
- Should `Chef`'s three-way notify timing (`:immediately` / `:delayed`, vs. Ansible/Puppet's two-state model) require a dedicated `notifies.timing` field in the IR, or can it be inferred per-target from `on: changed`?
- Should Terraform/ARM's `-target`-equivalent selective-apply gap be surfaced as an IR-level limitation, or excluded from the IR's tag/selective-apply feature entirely?
- Multi-repo plugin org layout (`meridian-ansible`, `meridian-puppet`, etc.) — timing of the split, once the SDK is proven.

---

## 14. Naming Decisions Log

| Decision | Value |
|---|---|
| Framework name | Meridian |
| Core language | Go |
| Repo name | `meridian-core` |
| Repo description | "A unified configuration DSL that compiles to Ansible, Puppet, Chef, DSC, Terraform, and ARM — write once, target any configuration management or infrastructure-as-code backend." |
