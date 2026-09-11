# Security Policy

## Reporting a vulnerability

Report privately through **GitHub Security Advisories** on this repository:
[Report a vulnerability](https://github.com/ZanattaMichael/meridian-core/security/advisories/new).

Do not open a public issue for a security report.

## What we commit to

| Stage | Target |
|---|---|
| Acknowledgement | 3 working days |
| Initial assessment and severity | 10 working days |
| Fix or documented mitigation, critical and high | 30 days |
| Fix or documented mitigation, medium and low | Next scheduled release |
| Public advisory | On fix release, or at 90 days, whichever is first |

The 90-day backstop applies whether or not a fix is ready. You will know when the issue
becomes public.

## In scope

Anything inside Meridian's own pipeline. In particular:

- A value marked `secret: true` reaching an emitted artifact in plaintext.
- A secret reaching logs, `meridian plan` output, or the run-state store.
- A path that defeats the structural exclusion of secrets from the literal-baking
  emission path.
- An escaping bug in `exec` allowing injection into a generated artifact.

## Out of scope

- The at-rest security of each target tool's own secrets mechanism: Ansible Vault key
  management, Puppet eyaml key distribution, Chef data-bag secrets, the certificate
  lifecycle behind MOF encryption.
- **Terraform storing `sensitive` values in plaintext in state.** This is known,
  documented, and warned about at plan time. It is a property of Terraform, not a
  Meridian vulnerability.
- The content of user-authored `exec` blocks.
- Weaknesses in a target tool itself. Please report those upstream.

## Safe harbour

Good-faith research following this policy will not be pursued. Test against your own
infrastructure only.

Reporters are credited in the advisory unless they ask not to be.

The reasoning behind all of the above, including the threat model, is in
[`docs/plans/security.md`](docs/plans/security.md).
