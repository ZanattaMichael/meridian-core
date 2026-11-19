# A cloud credential reaches a tool in its environment

| | |
|---|---|
| Area | Targets |
| Status | **proposed**: made during the work and merged, not yet accepted or changed by the owner |
| Made in | the pull request "Phase 7 part 7: cloud credentials on Terraform, OpenTofu, Pulumi, CloudFormation and Packer in the tool's environment" |
| Where it shows | [Credentials](../reference/dsl/credentials.md#reaching-a-cloud-as-a-credential), [Terraform](../targets/terraform.md#credentials), [OpenTofu](../targets/opentofu.md#credentials), [Pulumi](../targets/pulumi.md#credentials), [CloudFormation](../targets/cloudformation.md#credentials), [Packer](../targets/packer.md#credentials), [Secrets in memory](../concepts/secrets-in-memory.md#a-credential-in-a-run) |

## Context

An infrastructure document is applied to a cloud account, not to a host, and the identity
it is applied as is a credential of type `aws`, `azure` or `gcp`. The tools reach the cloud
through the provider's SDK, and the SDKs take a login in three ways: from the environment,
from their own files under the user's home directory (`~/.aws/credentials`, `~/.azure`,
`~/.config/gcloud`), or from the machine's metadata service. None reads standard input or a
file named on the command line, except for a Google service account key and an Azure
certificate, whose path the environment names. The [plan's table](../plans/language-expansion.md#116-handing-a-secret-to-a-tool)
ranks the environment fourth of four, allowed only where the tool has no other way, listed,
and reported by `plan`; it names provider credentials as that case.

## The choice

A cloud credential goes into the environment of the tool's process, and nowhere else. The
document names it in a new field, `spec.endpoint` (`type: cloud`, `credential: <name>`), on
either kind of document, as `targetHost.credential` names a host's; the core checks the
credential's type against the endpoint's kind and refuses the document for a target that
does not declare the `cloud_credentials` capability. The SDK does the handover once
(`meridian_sdk::cloud`): the plain fields and the secret fields become the providers' own
variables (`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, `ARM_CLIENT_ID` and
`ARM_CLIENT_SECRET`, `GOOGLE_PROJECT`, and the rest), a secret going from protected memory
into the child's environment block at the spawn and into no `String` of Meridian's; a
Google key or an Azure certificate is a `0600` file named by `GOOGLE_APPLICATION_CREDENTIALS`
or `ARM_CLIENT_CERTIFICATE_PATH`, removed with the stage. Every call of a stage is told,
since each is a process of its own that loads the providers. The plan line says `in the
tool's environment`, so a reviewer sees which stage goes the fourth way.

What the tool cannot take is refused at compile with the reason (`unsupported-credential`):
an `aws` `roleArn`, since the SDKs assume a role only through a profile that names it, and
a profile the credential can name instead; a plain field with a line break; `azure` on
`packer`, whose Azure plugin reads its login from the template, which the plugin does not
write one into; a `gcp` `impersonate` on `packer`; a credential for another cloud on
`cloudformation`. An ambient credential hands nothing secret and still tells the plain
fields, so `from: ambient` with a `profile` chooses the profile for the run.

`bicep` and `arm` take no cloud credential yet. The Azure command line signs in with
`az login --service-principal`, which takes the secret on its command line only; a route
through a certificate file and a private `AZURE_CONFIG_DIR` is possible and is left for
later. Until then a document that names one for them is refused before anything runs.

## Alternatives

- Writing a credentials file under a private directory and pointing the SDK at it
  (`AWS_SHARED_CREDENTIALS_FILE`, `AZURE_CONFIG_DIR`). Rejected for now: the file is rank 2
  and the pointer is still an environment variable, so the exposure to the same user is the
  same (the path, then the file), and only the AWS SDK takes a file this way; it may still
  be worth taking for `bicep` and `arm`, whose tool takes nothing else.
- Compiling the plain fields into the configuration (a `provider` block's `region`, a
  Packer source's `project_id`). Rejected: the providers read them from the environment
  as well, and a configuration that names an account is one that cannot be applied as
  another.
- Taking a `roleArn` by assuming the role in Meridian with the SDK and handing the session
  credentials over. Rejected here: Meridian would then hold an AWS client of its own; the
  profile route gives the SDK the same result with what it already reads.

## Status

Proposed, with the owner's direction of 2026-11-19 to be built:

- Credentials are kept in credential providers (a vault, a keychain, a cloud secret store).
  Meridian reads a credential from its provider when a run needs it and never stores one.
  Reading from those providers is not built yet; today a secret comes from `env` or a file.
- A credential goes on a tool's command line only when the document asks for that
  explicitly. The opt-in is not built yet.
- `az login` is not used. `bicep` and `arm` will authenticate through a credential
  provider such as workload identity federation, which hands the tool a short-lived token
  instead of a secret. Until then they refuse a cloud credential.
- Passwords are to be excluded entirely, since they cause more problems than they solve.
- The environment route above stays for the tools that read a login nowhere else.

Still open: which providers come first, and whether the plan line's wording is the right
amount of warning for rank 4.
