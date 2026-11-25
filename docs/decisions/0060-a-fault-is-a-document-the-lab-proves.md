# A fault is a document the lab proves per environment

| | |
|---|---|
| Area | Testing, orchestration |
| Status | **accepted**: the owner, 2026-11-25, with issue 237 |
| Made in | the pull request for issue 237 |
| Where it shows | [`meridian lab fault`](../reference/cli.md#lab-fault), [the fault catalogue](../reference/fault-catalogue.md), [troubleshooting](../operations/troubleshooting.md) |

- **A fault is a document** (`kind: Fault`, one per file under `lab/faults/`): the
  category it applies to, how the environment is broken (`inject`: a command on the
  endpoint, or a document applied outside the run's state), and what a run over the
  broken environment must report (`expect`: a failed run, the cause word, the resource,
  and — when #223 and #225 land — the probe and the known failure that must find it).
- **Proof is a run of phases**, like a lifecycle case: provision the environment, inject,
  check that the run fails the way `expect` says (cause word and resource, from the
  failures the run kept or its events), check `diagnose` names the resource first when the
  fault says so, then destroy whatever came before. Destroy always runs.
- **Injection runs where the environment runs.** On a real target the command runs on
  the endpoint over the stage's connection; on the fake target, whose machine is the
  scratch directory, the command runs there. A document injection is applied against the
  same state, so the tool's own record is changed the way the fault describes.
- **Data is read per phase**, not once: a fault may break the world by changing what the
  documents read, and the check must see the changed files.
- **`expect.probe` and `expect.knownFailure` are parsed but not asserted** until the
  engine probes (#223) and the known-failure registry (#225) exist; a fault names them
  so the assertions switch on without changing the files.
- **A fault with no finder is a gap**: every fault names the `diagnose` expectation or a
  probe; a catalogue test refuses one that names neither, and files the gap against #212.
