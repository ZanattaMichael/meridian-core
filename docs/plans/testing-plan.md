# Meridian — Testing & Integration Testing Plan

> Status: **accepted**. Originally authored in issue #2; this file is the canonical copy.
> Changes to this plan are made by pull request against this file, not by editing the issue.

This plan maps directly onto the architecture in `meridian-design-plan.md`: `ir → resolve → facts → condition → graph → transform → sort → validate → emit → runner → orchestrator`. Each stage gets its own test tier, plus cross-stage and cross-plugin tiers, because the majority of real bugs in this system will live **at the seams** (resolve→condition, transform→sort, plugin boundary, orchestrator handoff) rather than inside any single pure function.

---

## 1. Testing Philosophy

1. **Determinism is a testable property, not an assumption.** Any stage that touches ordering (resolve merge, graph sort, emit) must have tests that run N times and assert byte-identical output — not just "correct" output once.
2. **Capability claims are contracts.** If a plugin declares `Capabilities{NativeNotify: true}`, a shared contract test suite must pass against it. A plugin cannot ship without passing the contract tests for whatever it claims to support.
3. **No silent degradation, ever, in tests either.** Any test that hits an unsupported feature/target combination must assert a specific, typed error — not "doesn't crash."
4. **Fixtures are the source of truth for cross-target behavior.** The same IR fixture compiled against five plugins is the primary mechanism for catching drift between "what the IR says" and "what each target actually does."
5. **Real backends in CI, not just mocks, for at least one path per target.** Transform/sort/emit can be unit tested against golden files, but at least one full pipeline per target must run against a real (containerized) Ansible/Puppet/Terraform/etc. instance before release, because golden-file tests can't catch "the emitted HCL is syntactically valid but semantically wrong."

---

## 2. Test Pyramid

```
                        ┌─────────────────────┐
                        │   E2E / Release      │  slow, real backends, few
                        │   (Tier 5)            │
                        ├─────────────────────┤
                    ┌───┤  Integration          │  moderate, real CLIs in
                    │   │  (Tier 4)              │  containers, per-target
                    │   ├─────────────────────┤
                ┌───┤   │  Cross-plugin         │  fixture-driven, all
                │   │   │  contract (Tier 3)     │  plugins against shared IR
                │   ├───┴─────────────────────┤
            ┌───┤   │  Plugin unit             │  fast, per-stage,
            │   │   │  (Tier 2)                 │  per-plugin (transform/sort/emit)
            │   ├───┴─────────────────────────┤
        ┌───┤   │  Core unit                    │  fastest, pure functions:
        │   │   │  (Tier 1)                      │  ir, resolve, graph, condition
        └───┴───┴───────────────────────────────┘
```

Rough CI budget target: Tier 1–2 on every commit (<2 min), Tier 3 on every PR (<10 min), Tier 4 nightly + pre-merge to `main` (containerized, ~20–40 min), Tier 5 pre-release only (real cloud/agent infra, manually triggered or scheduled).

---

## 3. Tier 1 — Core Unit Tests

Pure functions, no I/O, no target awareness. These live in `internal/ir`, `internal/resolve`, `internal/facts`, `internal/condition`, `internal/graph`.

### 3.1 `internal/ir` — parsing & validation

- Valid `Infrastructure` and `ResourceSet` documents parse into expected structs.
- Malformed YAML/JSON produces a clear, located error (line/column where possible), not a panic.
- Schema validation rejects: unknown `type`, missing required `params`, `dependsOn` referencing a non-existent `id`, duplicate `id` within one document.
- `apiVersion` mismatch handling (forward-compat: unknown future version → explicit "unsupported apiVersion" error, not silent partial parse).
- `targetHost.from: infrastructure` references resolve to the correct document when multiple `Infrastructure`/`ResourceSet` docs are loaded together.

### 3.2 `internal/resolve` — hierarchical merge

- Each merge strategy independently: `first-found`, `deep-merge`, `unique-array-merge`.
- Precedence order is respected (`node` overrides `role` overrides `environment` overrides `common`).
- Deep-merge on nested maps doesn't clobber sibling keys not present in the overriding layer.
- Unique-array-merge dedupes correctly on both primitive and object array elements (define and test the dedup key for object elements explicitly — this is an easy silent-bug spot).
- Missing hierarchy layer file (e.g. no `role/webserver.yaml` exists) is either a no-op or an error, per configured strictness — **test both configured modes**.
- **Determinism test**: resolve the same node against the same hierarchy 100 times in a randomized-execution-order test harness (e.g. shuffle internal map iteration via `GOMAXPROCS`/parallel subtests) and assert identical output every time.
- Circular hierarchy reference (a templated hierarchy path that resolves back to itself) is a detected error, not infinite recursion.

### 3.3 `internal/facts` — gathering

- `FactProvider.Gather()` failures propagate with the provider name and node identity attached (not a bare error).
- Facts merge into resolved data under the `facts.*` namespace without colliding with hierarchy-sourced keys of the same name (test the collision case explicitly — decide and enforce a precedence rule).
- Timeout/unreachable-node handling: gather stage fails the specific node, not the whole batch, when gathering facts across multiple nodes concurrently.
- Mock `FactProvider` implementations for common providers (`file_exists`, `package_version`, `os_release`) with both "fact present" and "fact absent" cases.

### 3.4 `internal/condition` — evaluate & prune

- `when` expressions evaluate correctly against a resolved-data fixture (hierarchy-only, facts-only, and combined).
- A resource with no `when` is always kept.
- A resource whose `when` references an undefined variable is a compile-time error, not a silent `false`.
- Pruned resources are fully removed from the AST passed downstream (not just flagged/skipped — assert absence, not just an "excluded" marker, since downstream stages should never need to know a resource was ever considered).
- `dependsOn[].ifMissing: error` fails compilation with the referencing resource + missing target both named in the error.
- `dependsOn[].ifMissing: skip` removes the edge cleanly without breaking topological sort on the remaining graph.
- `notifies` edges pointing at a pruned resource follow the same `ifMissing` semantics as `dependsOn` — test this is not accidentally exempted.

### 3.5 `internal/graph` — build & sort

- Valid DAGs topologically sort correctly.
- Cycles are detected and reported with the **full cycle path** in the error (not just "a cycle exists somewhere").
- **Determinism / tiebreak test**: construct a graph with genuinely independent nodes (no edges between them), run topological sort 50+ times, assert identical output ordering every time, driven by declaration-index tiebreak — not by Go map iteration.
- `HardOrder` vs `Notify` edge kinds are distinguished correctly when both exist between overlapping resource pairs.
- Self-referencing `dependsOn` (a resource depending on itself) is rejected at graph-build time with a clear error.

---

## 4. Tier 2 — Plugin Unit Tests (per target, per stage)

Applied identically across `plugins/ansible`, `plugins/puppet`, `plugins/terraform`, `plugins/dsc-classic`, `plugins/dsc-v3`, `plugins/chef`. Each plugin owns its own test suite, but the **shape** of the suite is standardized so coverage gaps are visible at a glance.

### 4.1 `transform.go`

- Every entry in `SupportedResourceTypes()` has at least one test asserting correct native-type + param mapping.
- An IR resource type **not** in the support list produces a specific "unsupported resource type on this target" error — not a generic failure, not a best-effort guess.
- Param translation edge cases: missing optional params get target-appropriate defaults; state-value mapping (`present`/`running` → target vocabulary) is exhaustively tested for every declared state value.

### 4.2 `sort.go`

- Target-specific ordering semantics match the table in the design plan (Section 7) — e.g. explicitly assert Ansible's sort **flattens** to a strict sequence while Puppet's/DSC's **preserves** graph structure as metaparameters.
- Cycle propagation: a cycle that reaches the sort stage (shouldn't happen if Tier 1 graph tests are solid, but test defensively) fails loudly here too.
- Flattening-with-parallelism-loss: when independent resources exist, assert the plan-time warning is emitted for targets that lose parallelism info (Ansible), and **not** emitted for targets that preserve it (Puppet/DSC/Terraform).

### 4.3 `emit.go`

- **Golden file tests**: for each supported resource type (and realistic combinations), assert emitted output matches a checked-in golden file exactly.
- Emit performs **no logic** — test this indirectly by confirming identical AST input always produces identical output regardless of how it was constructed (i.e., emit is a pure function of its input, nothing hidden in template state).
- Every emitted artifact for every fixture must contain **zero conditional constructs** — automated check: grep/parse emitted output for target-native conditional syntax (`when:` in YAML output for Ansible beyond structural keys, `if` in Puppet manifests, etc.) and fail the test if found, since `when` should always have been pruned upstream.

### 4.4 `validate.go`

- Target-specific constraint violations are caught pre-emit: Puppet global title uniqueness, DSC resource name character restrictions, Terraform address collisions.
- Validation errors include the offending resource `id` and the specific rule violated.

### 4.5 `runner.go`

- Mocked CLI invocation: assert correct command/args construction for `apply`/`plan` per target, without actually shelling out (use an injectable command executor interface).
- Output/state parsing: feed canned real CLI output (captured once from a real run, checked in as a fixture) and assert `Outputs`/`Diff` parse correctly.
- Non-zero exit code handling surfaces the target tool's own error output, not a generic "command failed."

### 4.6 `Capabilities()` self-consistency check (runs for every plugin automatically)

A meta-test, run once per plugin as part of the standard suite:
- If `NativeNotify: true` or `SyntheticNotify: true`, the plugin **must** pass the shared notify contract suite (Tier 3).
- If `RuntimeCondition: true`, the plugin **must** correctly reject a `when`-pruned resource reaching its emitter (proving pruning still happened even though the plugin *could* have supported runtime evaluation) and correctly emit a target-native conditional when `runtimeWhen` is explicitly used.
- If a capability is `false`, a fixture that requires it must produce the documented hard-fail, tested explicitly per plugin (not just asserted once generically).

---

## 5. Tier 3 — Cross-Plugin Contract Tests

These live outside any single plugin's directory (e.g. `internal/contracttest/`) and run the **same fixture IR** against every plugin claiming support, asserting behavior consistent with the capability matrix — this is the primary defense against the "notify quietly means something different per target" class of bug.

### 5.1 Shared fixture library

Maintain a set of canonical IR fixtures, each targeting a specific cross-cutting concern:

| Fixture | Exercises |
|---|---|
| `fixture_package_service.yaml` | Baseline package/service + `dependsOn` — the "easy 80%" case, must pass identically-shaped on every target |
| `fixture_notify_changed.yaml` | `notifies` with `on: changed` — run against every plugin, assert native targets produce handler/subscribe-equivalent output, synthetic targets produce the documented workaround, and the workaround is flagged in plan output |
| `fixture_conditional_hierarchy.yaml` | `when` resolved purely from hierarchy data — assert pruning happens identically regardless of target (this should be target-invariant, a good regression guard) |
| `fixture_conditional_facts.yaml` | `when` resolved from gathered facts — same invariance assertion |
| `fixture_dangling_dependency.yaml` | A pruned resource with `ifMissing: error` and `ifMissing: skip` variants |
| `fixture_cyclic.yaml` | Deliberately cyclic graph — assert every plugin's compile fails with a cycle error (not a target-specific one, since this should fail before reaching any plugin) |
| `fixture_unsupported_type.yaml` | A resource type unsupported by a specific target (e.g. `service` against Terraform) — assert the documented hard-fail/escape-hatch behavior per target |
| `fixture_secret_value.yaml` | A `secret: true` value — assert it never appears in plaintext in any emitted artifact, for any target (this one is a security-relevant test, not just correctness — treat failures as high severity) |
| `fixture_multi_stage_handoff.yaml` | `Infrastructure` → `ResourceSet` output handoff (`targetHost.from`) — assert the resolved value is correctly injected pre-compile |

### 5.2 Capability-matrix assertion test

A single parameterized test that walks the full matrix from the design plan (Section 8) and, for every `(target, feature)` cell, asserts the fixture run produces exactly the declared outcome: native / synthetic-with-warning / hard-fail. This test is the living enforcement of the capability table — **if the table in the design doc changes, this test must change too**, and a mismatch between doc and test should be treated as a documentation bug, not just a test failure.

### 5.3 Determinism across the full pipeline

Run `fixture_package_service.yaml` (and 2-3 others) through the **entire** `resolve → condition → graph → transform → sort → emit` pipeline 20+ times per target, assert byte-identical emitted artifacts every time. This is the highest-value determinism test since it exercises real stage composition, not just one function in isolation.

### 5.4 DSC classic vs. DSC v3 divergence tests

Explicit tests asserting the two DSC plugins are **not** assumed to share behavior:
- Same fixture compiled against both, asserting different (not accidentally identical) output shape where the models genuinely differ (MOF `Configuration` block vs. native JSON/YAML).
- `notifies` fixture run against both — if DSC v3 turns out to have a native change-detection primitive (per the open question in the design doc), this test is where that gets encoded once resolved, replacing an assumed-synthetic result with a confirmed one.

---

## 6. Tier 4 — Integration Tests (real CLIs, containerized)

These invoke actual target tool binaries against actual (ephemeral, containerized) hosts — no more mocking `runner.go`. Runs nightly and pre-merge to `main`, not on every commit, given the cost.

### 6.1 Environment

- Docker Compose (or equivalent) stack per target: an Ansible control container + SSH-reachable target container; a Puppet server + agent pair; a DSC LCM-capable Windows container (or a licensed CI runner, since Windows containers for DSC testing are the most likely to need special CI infrastructure — budget for this explicitly); a `dsc` v3 binary against a Linux target; a Terraform container against a local/mocked cloud provider (e.g. LocalStack for AWS-shaped resources, or the `null`/`local` providers for provider-agnostic cases); a Chef Infra Client + local Chef Zero server.
- Each environment is disposable and rebuilt per test run — no persistent state carried between runs, to keep tests independent of prior test ordering.

### 6.2 Per-target integration suite (mirrors the fixture library from Tier 3, but actually applied)

For each target, take the Tier 3 fixtures and:
1. Compile via Meridian (already covered by Tier 3 — reuse the artifact).
2. **Actually apply** the artifact against the containerized target using the target's real CLI.
3. Assert real-world end state: is the package actually installed, is the service actually running, does the file actually exist with correct content.
4. **Re-apply the same artifact** and assert idempotency: second apply reports no changes (or the target-appropriate equivalent — e.g. Ansible reports `changed: 0`, Terraform plan shows no diff, Puppet catalog reports no corrective changes).
5. For `notifies` fixtures specifically: apply once (handler/restart should fire), apply again with no underlying change (handler should **not** fire) — this is the test that actually proves the synthetic DSC/Terraform workarounds work as designed, not just that they compile.

### 6.3 Two-stage orchestrator integration test

The most important integration test in the whole plan, since it's the seam most likely to have bugs that no single-plugin test can catch:

1. Run `Infrastructure` stage against a real (or LocalStack-mocked) provisioner — creates a container standing in for "the VM."
2. Assert the orchestrator correctly reads back `web_vm_ip` from Terraform state/output.
3. Assert fact-gathering runs against the newly-provisioned node and succeeds (proving the reachability-before-gather ordering from the design plan actually holds).
4. Assert the `ResourceSet` stage compiles using the gathered facts + hierarchy data, and the emitted artifact is fully static (no conditionals — reuse the Tier 2.3 check here too).
5. Apply the `ResourceSet` stage and verify end state on the provisioned node.
6. **Failure-injection variant**: kill the node between Infrastructure apply and ResourceSet apply — assert the orchestrator fails clearly at the gather step rather than producing a compiled artifact against stale/absent facts.

### 6.4 Secrets handoff integration test

Pass a secret value through `Infrastructure` output → `ResourceSet` input, and assert:
- It's masked in Meridian's own logs/plan output.
- It's not written in plaintext to Meridian's run-state store.
- It correctly reaches the target's native secrets mechanism (e.g. actually lands in an Ansible Vault-encrypted var, not a plaintext task param) — this test should fail loudly if a future change accidentally routes a secret through the "bake resolved values in as literals" default path from Section 5 of the design plan, since that path is explicitly wrong for secrets.

### 6.5 Drift-asymmetry documentation tests

Not bug-catching tests so much as **living documentation enforced by CI**: for each target, a test that provisions a resource, manually drifts it (e.g. SSH in and stop the service), then runs Meridian's `plan` again and asserts the *documented* behavior — Terraform detects it, Ansible/Chef don't report drift without a fresh apply, Puppet/DSC agents self-correct on their next poll interval. If a target's actual drift behavior ever changes (tool upgrade, etc.), this test catches the doc going stale.

---

## 7. Tier 5 — End-to-End / Release Tests

Run before tagging a release, against real (non-mocked) cloud infrastructure and real target agents, not containers standing in for them.

- Full `Infrastructure` (real cloud provider, a real low-cost VM) → `ResourceSet` (real Ansible/Puppet/DSC/Chef apply) run for at least one representative fixture per target.
- Multi-node fixture: one `ResourceSet` applied across several nodes concurrently, verifying the orchestrator's concurrency model doesn't cross-contaminate state between nodes (facts gathered for node A never leak into node B's compile).
- A **DSC v3 real Windows/Linux target** run specifically, since this is the newest and least-precedented plugin — treat it as higher-risk than the others by default until it has a track record.
- Upgrade-path test: run Meridian's own version N against artifacts/state produced by version N-1, to catch orchestrator run-state schema breakage early (this matters once the run-state store from Section 11.5 of the design plan is in use in the wild).

---

## 8. Non-Functional Test Concerns

| Concern | Approach |
|---|---|
| **Performance** | Benchmark `resolve` and `graph` sort against large fixture sets (1000+ resources, deep hierarchy) — these are the stages most likely to degrade non-linearly if a merge or sort algorithm is implemented naively |
| **Plugin SDK compatibility** | A "reference plugin" (minimal, intentionally trivial target) built and tested purely against the public `pkg/sdk` interface, with no access to `internal/`, to prove third parties can actually build a working plugin from the SDK alone — this test would have caught an SDK boundary leak long before an external contributor does |
| **Schema/doc drift** | Automated check that `schema/` (JSON Schema for the DSL) accepts every fixture in the shared fixture library and rejects a set of deliberately invalid fixtures — schema and fixtures must be kept in lockstep in CI, not just by convention |
| **Security** | The secrets-handoff test (6.4) plus a dedicated fuzz/static-scan pass over emitted artifacts across all fixtures, checking for any plaintext pattern matching known secret fixture values |
| **Error message quality** | A dedicated test file asserting specific error message content (not just error *presence*) for the highest-friction failure modes: cycle detection, unsupported resource type, dangling dependency, capability mismatch — these are the errors real users will hit most often, so wording quality is worth testing directly, not left to chance |

---

## 9. CI Wiring Summary

| Tier | Trigger | Real backends? | Target runtime |
|---|---|---|---|
| 1 — Core unit | every commit | No | < 30s |
| 2 — Plugin unit | every commit | No (mocked CLI) | < 90s |
| 3 — Cross-plugin contract | every PR | No | < 5 min |
| 4 — Integration | pre-merge to `main` + nightly | Yes (containerized) | 20–40 min |
| 5 — E2E / release | pre-release (manual/scheduled) | Yes (real infra) | hours, budget accordingly |

---

## 10. Open Items to Resolve Before Implementation

- Which containerized DSC environment is realistic for CI (Windows containers are the likely cost/complexity outlier — decide early whether DSC classic integration tests run in CI proper or are gated to a separate, less-frequent job).
- Whether LocalStack (or similar) is sufficient for Terraform/ARM integration tests, or whether a real low-cost cloud account is needed even at Tier 4, not just Tier 5.
- Ownership of the shared fixture library (Section 5.1) — these fixtures are effectively a second, test-facing spec of the IR's behavior and should be reviewed with the same rigor as the schema itself when the IR changes.
