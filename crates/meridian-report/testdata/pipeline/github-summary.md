## ✗ Meridian apply failed: 3 steps, 1 failed, 1 not run, 2 resources changed

- **Run:** 2026-11-18T09-14-03Z-7f3a
- **Command:** apply site.yaml
- **Started:** 2026-11-18T09:14:03.100Z
- **Duration:** 3.1 s

| Step | Target | Endpoint | Status | Changes | Duration |
|---|---|---|---|---|---|
| Infrastructure network | fake |  | ✓ succeeded | 1 changed | 1.3 s |
| ResourceSet web | fake | 10.0.0.5 | ✗ failed | 1 changed | 1.4 s |
| ResourceSet monitoring | fake |  | – not-run | none |  |

### What needs attention

- **ResourceSet web** failed: the tool reported a failure
- **ResourceSet monitoring** not run: the earlier step web failed

<details><summary>Infrastructure network: 1 changed</summary>

- changed create <code>vm</code>
- output <code>address</code>
- output <code>db_password</code> (sensitive)

</details>

<details><summary>ResourceSet web: 1 changed</summary>

- changed update <code>motd</code>

</details>

Once the cause is fixed, `meridian apply --resume` with the same documents continues from the step that failed.

<sub>Values are never in a report: it names steps, resources and outputs only.</sub>

The record is kept in .meridian (local): `meridian report 2026-11-18T09-14-03Z-7f3a` reads it back.
