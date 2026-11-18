# Plugin signatures

`meridian` can refuse to start a plugin binary not signed by a trusted key. A released
`meridian` does this by default for its own plugins.

## What is checked

A signed plugin has a file beside it with the same name plus `.sig`:

```
meridian-target-terraform
meridian-target-terraform.sig
```

The `.sig` holds an Ed25519 signature over the binary's SHA-256 and the key id. Before each
start, `meridian` hashes the binary and verifies it against a trusted key, offline. It proves
who signed the binary, not that it is safe.

## The policy

`--plugin-signatures`, or `MERIDIAN_PLUGIN_SIGNATURES`, says what to do:

| Policy | A plugin with a signature from a trusted key | Any other plugin |
|---|---|---|
| `require` | started | not started, and the command fails saying why |
| `warn` | started | started, with a warning on stderr |
| `off` | started | started; signatures are not looked at |

Default: `require` for a release, `off` for a source build. The
[language server](../tooling/language-server.md#plugin-signatures) follows it too, and holds
editor-named plugin directories to `require` unless given `--trust-editor-plugin-dirs`.

`meridian plugins` shows what the check found for each plugin:

```
terraform
  ...
  binary:    /home/me/.meridian/bin/meridian-target-terraform
  signature: verified, signed by key 780001b5de1d7691
```

The other values are `unsigned`, `invalid: <why>` and `not checked`.

## Which keys are trusted

**The release's own key.** Each release build makes a key pair, signs its plugins, embeds the
public key (also shipped as `release-key.pub`) and deletes the secret key. Plugins from
another release or platform are refused; install a release whole. To check `meridian`
itself, see [Check what you downloaded](../quickstart/install.md#check-what-you-downloaded).

**Keys you name.** `--trusted-key FILE` (repeatable; a directory means every `*.pub`), or
`MERIDIAN_TRUSTED_KEYS` separated by `:` (`;` on Windows).

```
meridian plugins --trusted-key ~/keys/acme.pub
export MERIDIAN_TRUSTED_KEYS=/etc/meridian/trusted-keys
```

## Using a plugin from somewhere else

Under `require`, a plugin not signed by the release key is refused:

```
error: plugin /opt/plugins/meridian-target-acme: the plugin was not started: it is not signed: ...
```

- Preferred: trust the author's public key with `--trusted-key`.
- Sign it with your own key and trust that.
- Use `--plugin-signatures warn`.

Keep it in its own `--plugin-dir`; install scripts replace their own directory.

## Signing plugins of your own

Make a key pair once:

```
meridian signing keygen --secret acme.key --public acme.pub
```

Keep `acme.key` private (written `0600`, never overwritten); publish `acme.pub`. Sign each
binary after its last change (strip, compress, code-sign):

```
meridian signing sign --key acme.key meridian-target-acme
```

Ship `meridian-target-acme.sig` beside it, per platform. To verify:

```
meridian signing verify --trusted-key acme.pub meridian-target-acme
meridian-target-acme: verified, signed by key 5f1c0a94e2b7d310
```

Non-zero if any fails.

## File formats

| File | Content |
|---|---|
| public key | `meridian-public-key-v1 <64 hexadecimal digits>`; anything after the key on the line is a comment |
| secret key | `meridian-secret-key-v1 <64 hexadecimal digits>` |
| signature | four lines: `meridian-plugin-signature-v1`, `key <id>`, `sha256 <digest>`, `sig <128 hexadecimal digits>` |

A key id is the first 16 hex digits of the public key's SHA-256. The signed message is
`meridian-plugin-signature-v1\n<lower-case hex sha256>\n`. The file's `sha256` line only
improves messages; the check hashes the binary.

## What this does not cover

- Someone who can replace `meridian` can replace the key.
- A swap between check and start is not caught.
- Built-in targets have no signature of their own.

## Using the check from other code

Crate `meridian-signing`: `SecretKey::sign_file`, `verify_file`, `Verification::check`
(`Policy`: off, warn, require), and `sign_bytes`/`verify_bytes`, used for
[module signatures](modules.md#signatures) (prefix `meridian-statement-signature-v1`, so the
two kinds never cross). Re-exported as `meridian_plugin_host::signature`.
