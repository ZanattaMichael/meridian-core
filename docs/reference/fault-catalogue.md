# The fault catalogue

`lab/faults/<name>.yaml`, one [`Fault`](../operations/lab.md#faults) document
per file: how the fault is injected, and what a run over the broken
environment must report. `meridian lab fault` proves each one on the fake
target today; the self-hosted lab runner brings the real machines and clouds
([issue 238](https://github.com/ZanattaMichael/meridian-core/issues/238)).

Every fault names the command that must find it — the `diagnose` expectation
or a probe — or it is a gap, filed against
[issue 212](https://github.com/ZanattaMichael/meridian-core/issues/212); a
test refuses a catalogue entry that names neither.

| Fault | Category | Injected by | A run must report | Found by |
|---|---|---|---|---|
| [bad-input-reaches-the-tool](../../lab/faults/bad-input-reaches-the-tool.yaml) | infrastructure | a command changes a value the documents read | `resource-failed` on the tool's inputs | `meridian diagnose` names it first |

`expect.probe` and `expect.knownFailure` are parsed but not asserted until
[issue 223](https://github.com/ZanattaMichael/meridian-core/issues/223)
(engine probes) and [issue 225](https://github.com/ZanattaMichael/meridian-core/issues/225)
(known failures per engine) land; a fault names them so the assertions switch
on without changing the files.

See [decision 0060](../decisions/0060-a-fault-is-a-document-the-lab-proves.md).
