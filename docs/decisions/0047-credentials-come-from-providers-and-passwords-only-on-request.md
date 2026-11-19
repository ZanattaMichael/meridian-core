# Credentials come from providers, and passwords only on request

| | |
|---|---|
| Area | Secrets |
| Status | **accepted**: the owner's direction of 2026-11-19; the parts below are not built yet |
| Made in | the project thread "Expand the Meridian DSL language" |
| Where it shows | [Secrets](../reference/dsl/secrets.md), [Credentials](../reference/dsl/credentials.md), [0029](0029-a-cloud-credential-reaches-a-tool-in-its-environment.md) |

## Context

A secret already comes from a provider that the document names: `env`, `file`, `exec`,
`keychain`, `keyring`, `secret-service` or `systemd-creds`. Meridian never stores one. The
owner set three rules on top of that:

- Credentials are defined and stored in the credential provider that suits them. Meridian
  reads from that provider when a run needs the credential, and stores nothing.
- A credential reaches a tool on its command line only when the document asks for that
  explicitly. `az login` is not used.
- A password is never stored or handled unless the document explicitly asks for it.

## The choice

1. **More built-in providers**: HashiCorp Vault, AWS Secrets Manager, Azure Key Vault, GCP
   Secret Manager, PowerShell SecretManagement and KeePass files. Each reads one value per
   reference and keeps it in protected memory, as the existing providers do.
2. **Provider plugins**: any other store is added as a plugin, the way a target is. The
   plugin answers "give me this value" over the plugin protocol and holds no state.
3. **Passwords on request**: a `usernamePassword` credential, or a `password` or
   `passphrase` field, is refused at validate unless the credential says
   `allowPassword: true`. The plan line names a credential that does. Using a password is
   not best practice: the reference docs and the plan warning say so and point to a key,
   a token or a federated login instead.
4. **Command line on request**: a credential that a tool can only take on its command line
   is refused unless the credential says `allowCommandLine: true`. The plan line names it
   and, as for passwords, the docs mark it as not best practice.
5. **Federated cloud logins**: a cloud credential can be a workload identity federation
   token (an OIDC token file the provider exchanges for a short-lived login). This is how
   `bicep` and `arm` will authenticate, instead of `az login`.

## Alternatives

- Keep only `exec` for third-party stores. It works for any store with a command line, but
  leaves the value in a child process's output, and gives a document no checked fields.
- Drop passwords entirely. The owner chose explicit opt-in instead.

## Status

Accepted as direction. Each part lands in its own pull request with tests and docs.
